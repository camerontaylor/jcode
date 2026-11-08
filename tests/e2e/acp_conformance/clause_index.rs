use super::*;

/// Keep stable clause references reviewable: every ID in the authoritative
/// contract has exactly one definition and a disposition in traceability.
#[test]
fn acp_conformance_clause_index_has_no_missing_or_duplicate_ids() -> Result<()> {
    let contract = include_str!("../../../docs/ACP_FORK_INTERFACE.md");
    let traceability = include_str!("TRACEABILITY.md");
    let mut ids = std::collections::BTreeSet::new();
    for fragment in contract.split("<!-- [").skip(1) {
        let (id, _) = fragment
            .split_once("] -->")
            .context("closed clause marker")?;
        assert!(id.starts_with("C-"), "unexpected clause: {id}");
        assert!(ids.insert(id), "duplicate clause: {id}");
        assert!(
            traceability
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .any(|token| token == id),
            "unmapped clause: {id}"
        );
    }
    assert!(ids.len() > 100, "index includes lists and table rows");
    Ok(())
}
