use super::*;

impl DaemonSession {
    /// Negotiate on a private replacement transport before publishing its
    /// writer. Only Ping and Subscribe are sent here, never uncertain requests.
    pub(super) async fn reconnect_transport(
        &self,
    ) -> Result<(BufReader<ReadHalf>, Vec<ServerEvent>)> {
        let socket = self
            .socket_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Transport has no reconnect address"))?;
        loop {
            anyhow::ensure!(self.router.lock().await.can_reconnect(), "View is closed");
            let attempt = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.reconnect_once(socket),
            )
            .await;
            match attempt {
                Ok(Ok(transport)) => return Ok(transport),
                Ok(Err(err)) if err.downcast_ref::<std::io::Error>().is_none() => return Err(err),
                Ok(Err(err)) => {
                    crate::logging::debug(&format!("acp: retrying reconnect transport: {err:#}"))
                }
                Err(err) => crate::logging::debug(&format!(
                    "acp: retrying stalled reconnect handshake: {err}"
                )),
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    async fn reconnect_once(
        &self,
        socket: &Path,
    ) -> Result<(BufReader<ReadHalf>, Vec<ServerEvent>)> {
        let stream = crate::server::connect_socket(socket).await?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let ping_id = self.next_id();
        write_reconnect_request(&mut writer, &Request::Ping { id: ping_id }).await?;
        let capabilities = loop {
            if let ServerEvent::Pong {
                id, capabilities, ..
            } = read_reconnect_event(&mut reader).await?
                && id == ping_id
            {
                break DaemonCapabilities::from_pong(&capabilities);
            }
        };
        let id = self.next_id();
        let target = self.session_id().await;
        write_reconnect_request(
            &mut writer,
            &Request::Subscribe {
                id,
                system_prompt: None,
                supports_pdf_panels: false,
                supports_user_response: false,
                supports_server_failover: true,
                supports_turn_identity: capabilities.turn_origin,
                crash_on_disconnect: false,
                continue_on_disconnect: false,
                // Resolve the existing target's cwd on the daemon.
                // Supplying a cwd permits Subscribe's new-session
                // fallback when the target disappeared.
                working_dir: None,
                selfdev: None,
                target_session_id: Some(target.clone()),
                client_instance_id: Some(self.instance_id.clone()),
                client_has_local_history: false,
                history_base: None,
                allow_session_takeover: false,
                terminal_env: crate::terminal_launch::snapshot_client_terminal_env(),
                client_name: None,
            },
        )
        .await?;
        let mut events = Vec::new();
        let mut matched_history = false;
        loop {
            let event = read_reconnect_event(&mut reader).await?;
            match &event {
                ServerEvent::Error {
                    id: reply, message, ..
                } if *reply == id => anyhow::bail!("{message}"),
                ServerEvent::History { session_id, .. } => {
                    anyhow::ensure!(session_id == &target, "Reconnect changed session");
                    matched_history = true;
                    apply_catalog_state(&mut *self.ui_state.lock().await, &event);
                }
                ServerEvent::Done { id: reply } if *reply == id => break,
                _ => {}
            }
            anyhow::ensure!(
                events.len() < MAX_BUFFERED_PEER_ACTIONS,
                "Reconnect snapshot overflow"
            );
            events.push(event);
        }
        anyhow::ensure!(matched_history, "Reconnect omitted session history");
        let mut router = self.router.lock().await;
        anyhow::ensure!(router.can_reconnect(), "View closed during reconnect");
        router.preserve_work = capabilities.preserve_work;
        router.set_turn_origin(capabilities.turn_origin);
        router.set_soft_interrupt_ids(capabilities.soft_interrupt_ids);
        *self.writer.lock().await = writer;
        router.transport_restored();
        Ok((reader, events))
    }
}

async fn write_reconnect_request(writer: &mut WriteHalf, request: &Request) -> Result<()> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_reconnect_event(reader: &mut BufReader<ReadHalf>) -> Result<ServerEvent> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Reconnect transport closed",
            )
            .into());
        }
        if let Some(event) = decode_daemon_line(&line) {
            return Ok(event);
        }
    }
}
