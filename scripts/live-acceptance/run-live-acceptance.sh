#!/usr/bin/env bash
# One entry point for live v1 acceptance of jcode through the real Paseo ACP
# connector plus a direct live ACP companion probe.
#
#   JCODE_E2E_BINARY=/abs/path/to/jcode scripts/live-acceptance/run-live-acceptance.sh
#
# Optional:
#   PASEO_CONSUMER_ROOT   consumer checkout, absolute or relative (default:
#                         sibling checkout ../paseo-v1-acceptance next to this
#                         repository). Resolved to its physical absolute path
#                         before any harness or evidence path uses it.
#   JCODE_ACCEPTANCE_OUT_DIR  evidence directory (default:
#                         $JCODE_SCRATCH_DIR/live-acceptance/... when set,
#                         otherwise <repo>/target/live-acceptance/...)
#   JCODE_EXPECTED_PROVENANCE substring that must appear in `jcode --version`
#   JCODE_ACCEPTANCE_ONLY connector|probe|barriers
#
# The harness creates its own daemon socket, JCODE_HOME, JCODE_RUNTIME_DIR and
# Paseo home. It never touches the shared daemon or user config.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
consumer_root="${PASEO_CONSUMER_ROOT:-$(cd "$repo_root/.." && pwd)/paseo-v1-acceptance}"
binary="${JCODE_E2E_BINARY:-}"
only="${JCODE_ACCEPTANCE_ONLY:-all}"

case "$only" in
  all | connector | probe | barriers) ;;
  *) printf 'live-acceptance: JCODE_ACCEPTANCE_ONLY must be all, connector, probe or barriers (got %s)\n' "$only" >&2; exit 2 ;;
esac

fail() {
  printf 'live-acceptance: %s\n' "$*" >&2
  exit 1
}

[[ -n "$binary" ]] || fail "set JCODE_E2E_BINARY to an absolute current jcode executable"
[[ "$binary" = /* ]] || fail "JCODE_E2E_BINARY must be an absolute path: $binary"
[[ -x "$binary" ]] || fail "JCODE_E2E_BINARY is not executable: $binary"
[[ -d "$consumer_root" ]] || fail "consumer checkout not found: $consumer_root"
# A caller may pass a relative root (for example ../paseo-v1-acceptance).
# Resolve the physical checkout before anything else uses it: the harnesses
# hand it to createRequire, which needs an absolute path, and evidence must
# record the real checkout even when it was named through a symlink.
resolved_consumer_root="$(cd -- "$consumer_root" && pwd -P)" ||
  fail "consumer checkout is not reachable: $consumer_root"
consumer_root="$resolved_consumer_root"
[[ -x "$consumer_root/node_modules/.bin/tsx" ]] || fail "consumer checkout has no tsx: $consumer_root"
[[ -f "$consumer_root/plugins/jcode-provider/index.server.ts" ]] || fail "consumer jcode provider plugin missing"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -n "${JCODE_ACCEPTANCE_OUT_DIR:-}" ]]; then
  out_dir="$JCODE_ACCEPTANCE_OUT_DIR"
elif [[ -n "${JCODE_SCRATCH_DIR:-}" ]]; then
  out_dir="$JCODE_SCRATCH_DIR/live-acceptance/live-run-$stamp"
else
  out_dir="$repo_root/target/live-acceptance/live-run-$stamp"
fi
mkdir -p "$out_dir"

version="$("$binary" --version 2>&1 | head -1)"
if command -v sha256sum >/dev/null 2>&1; then
  digest="$(sha256sum "$binary" | awk '{print $1}')"
else
  digest="$(shasum -a 256 "$binary" | awk '{print $1}')"
fi
consumer_head="$(git -C "$consumer_root" rev-parse HEAD)"
consumer_branch="$(git -C "$consumer_root" branch --show-current)"
consumer_dirty="$(git -C "$consumer_root" status --porcelain | wc -l | tr -d ' ')"

if [[ -n "${JCODE_EXPECTED_PROVENANCE:-}" && "$version" != *"$JCODE_EXPECTED_PROVENANCE"* ]]; then
  fail "binary provenance mismatch: '$version' does not contain '$JCODE_EXPECTED_PROVENANCE'"
fi
if [[ "$version" == *"900265ac7"* ]]; then
  printf 'live-acceptance: WARNING: binary is the stale 2026-11-09 build (900265ac7); evidence is not current\n' >&2
fi

{
  printf 'repo_root=%s\n' "$repo_root"
  printf 'binary=%s\n' "$binary"
  printf 'binary_version=%s\n' "$version"
  printf 'binary_sha256=%s\n' "$digest"
  printf 'consumer_root=%s\n' "$consumer_root"
  printf 'consumer_head=%s\n' "$consumer_head"
  printf 'consumer_branch=%s\n' "$consumer_branch"
  printf 'consumer_dirty_files=%s\n' "$consumer_dirty"
  printf 'out_dir=%s\n' "$out_dir"
  printf 'node=%s\n' "$(node --version)"
  printf 'node_conditions=source\n'
  printf 'started_utc=%s\n' "$stamp"
} | tee "$out_dir/provenance.txt"

# Surface failed scenario names and bounded errors from the evidence file.
# CI discards the runner's filesystem, so the job log must carry the reason.
# The formatter is shared with the harnesses (report-evidence.ts); this wrapper
# only decides which file to report.
report_evidence_failures() {
  local name="$1"
  local evidence="$out_dir/$name.evidence.json"
  if [[ ! -f "$evidence" ]]; then
    printf 'FAILED %s: harness exited without writing evidence (%s)\n' "$name" "$evidence" >&2
    return 0
  fi
  (
    cd "$consumer_root"
    node --conditions=source --import tsx \
      "$repo_root/scripts/live-acceptance/report-evidence.ts" "$evidence" "$name"
  ) >&2
}

run_one() {
  local name="$1"
  local harness="$2"
  printf '\n== %s ==\n' "$name"
  set +e
  (
    cd "$consumer_root"
    PASEO_CONSUMER_ROOT="$consumer_root" \
    JCODE_ACCEPTANCE_OUT="$out_dir/$name.evidence.json" \
      node --conditions=source --import tsx "$harness"
  ) 2>&1 | tee "$out_dir/$name.log"
  local status="${PIPESTATUS[0]}"
  set -e
  if [[ "$status" -ne 0 ]]; then
    report_evidence_failures "$name"
  fi
  printf '%s exit=%s\n' "$name" "$status" | tee -a "$out_dir/summary.txt"
  return "$status"
}

status=0
if [[ "$only" == "all" || "$only" == "connector" ]]; then
  run_one connector "$repo_root/scripts/live-acceptance/jcode-live-acceptance.ts" || status=1
fi
if [[ "$only" == "all" || "$only" == "probe" ]]; then
  run_one probe "$repo_root/scripts/live-acceptance/jcode-acp-probe.ts" || status=1
fi
if [[ "$only" == "all" || "$only" == "barriers" ]]; then
  run_one barriers "$repo_root/scripts/live-acceptance/jcode-live-barriers.ts" || status=1
fi

printf '\nevidence directory: %s\n' "$out_dir"
printf 'connector evidence: %s\n' "$out_dir/connector.evidence.json"
printf 'probe evidence:     %s\n' "$out_dir/probe.evidence.json"
printf 'barrier evidence:   %s\n' "$out_dir/barriers.evidence.json"
exit "$status"
