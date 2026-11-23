#!/usr/bin/env bash
# AF-08: end-to-end mesh harness.
#   --mock-only  CI-safe: temp HOME/config/db, mock peers + file-based fake window (default)
#   --real       manual: needs tmux + real CLIs + a D-Bus session bus
set -euo pipefail

MODE="${1:---mock-only}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OCTO_BIN="${OCTO_BIN:-$ROOT_DIR/target/debug/octo}"

if [[ ! -x "$OCTO_BIN" ]]; then
  if command -v octo >/dev/null 2>&1; then
    OCTO_BIN="$(command -v octo)"
  else
    echo "FAIL build: octo binary not found (run cargo build --workspace first)" >&2
    exit 1
  fi
fi

expect_contains() {
  # expect_contains <needle> <output>  (captured output avoids SIGPIPE from `cmd | grep -q`)
  if ! grep -qF -- "$1" <<<"$2"; then
    echo "FAIL: expected output to contain: $1" >&2
    echo "----- actual output -----" >&2
    printf '%s\n' "$2" >&2
    exit 1
  fi
}

run_mock_only() {
  local tmp_root
  tmp_root="$(mktemp -d)"
  trap 'rm -rf "$tmp_root"' RETURN

  export HOME="$tmp_root/home"
  mkdir -p "$HOME/.config/octomesh" "$HOME/.octomesh" "$tmp_root/proj_a" "$tmp_root/proj_b" \
    "$tmp_root/xdg/octomesh"
  export OCTOMESH_CONFIG="$HOME/.config/octomesh/octomesh.toml"
  export OCTOMESH_DB="$HOME/.octomesh/state.db"
  export OCTOMESH_FAKE_WINDOW_DIR="$tmp_root/fake_window"
  export XDG_CONFIG_HOME="$tmp_root/xdg"

  cat >"$tmp_root/xdg/octomesh/connectors.toml" <<'EOF'
[connectors.github]
command = "npx"
[connectors.drive]
command = "npx"
[connectors.gitlab]
command = "npx"
EOF

  cat >"$OCTOMESH_CONFIG" <<EOF
adapter_runner = ["python3", "$ROOT_DIR/adapters/run_adapter.py"]
max_depth = 2
default_cooldown_secs = 60

[mesh]
project = "proj_a"
presence_ttl_secs = 120
busy_ttl_secs = 300

[project]
name = "proj_a"
allowed_connectors = ["github", "drive"]

[budgets]
default_max_prompt_chars = 20000

[peers.mock_primary]
adapter = "mock"
timeout_secs = 5

[peers.codex]
adapter = "mock"
timeout_secs = 5

[peers.mock_backup]
adapter = "mock"
timeout_secs = 5
max_prompt_chars = 100

[peers.fake_window]
adapter = "dbus"
command = "$ROOT_DIR/scripts/fake_window.sh"
args = ["bus=org.example.FakeBus", "path=/org/example/FakeObj", "method=org.example.Fake.sendText"]
timeout_secs = 5
window_bus = "org.example.FakeBus"
window_path = "/org/example/FakeObj"

[roles]
demo = ["mock_primary", "mock_backup"]
EOF

  # a. AF-01: status + doctor from an isolated project dir
  (cd "$tmp_root/proj_a" && "$OCTO_BIN" status >/dev/null && "$OCTO_BIN" doctor >/dev/null)
  echo "PASS AF-01: status + doctor from isolated project dir"

  # b. AF-02: mailbox project isolation + ack
  (cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" send codex "hello proj_a" --project proj_a >/dev/null)
  (cd "$tmp_root/proj_b" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" send codex "hello proj_b" --project proj_b >/dev/null)
  INBOX_A="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" inbox --peer codex --project proj_a)"
  expect_contains "hello proj_a" "$INBOX_A"
  if grep -qF "hello proj_b" <<<"$INBOX_A"; then
    echo "FAIL AF-02: project isolation leaked" >&2
    exit 1
  fi
  MSG_ID="$(grep -oP '#\K[0-9]+' <<<"$INBOX_A" | head -1)"
  ACK_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" inbox --peer codex --project proj_a --ack "$MSG_ID")"
  expect_contains "acked 1" "$ACK_OUT"
  INBOX_ALL="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" inbox --peer codex --project proj_a --all)"
  expect_contains "(ack)" "$INBOX_ALL"
  echo "PASS AF-02: mailbox project isolation + ack"

  # c. AF-03: presence auto-registration on client start
  (cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=codex "$OCTO_BIN" status >/dev/null)
  PRESENCE="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" presence)"
  expect_contains "codex" "$PRESENCE"
  expect_contains "LIVE" "$PRESENCE"
  echo "PASS AF-03: presence auto-registration"

  # d. AF-04: window delivery -> task row delivered -> ack -> accepted
  (cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" ask fake_window "inspect the mesh" >/dev/null)
  TASKS_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" tasks --peer fake_window --status delivered)"
  TASK_ID="$(grep -oP '"id": "\K[^"]+' <<<"$TASKS_OUT" | head -1)"
  if [[ -z "$TASK_ID" ]]; then
    echo "FAIL AF-04: no delivered task for fake_window" >&2
    (cd "$tmp_root/proj_a" && "$OCTO_BIN" tasks --limit 10) >&2 || true
    exit 1
  fi
  grep -q "task=$TASK_ID" "$OCTOMESH_FAKE_WINDOW_DIR/tasks.log"
  ACK_TASK="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" ack "$TASK_ID")"
  expect_contains '"accepted"' "$ACK_TASK"
  echo "PASS AF-04: window delivery + task=<id> header + ack"

  # e. failover: role demo, primary rate-limits, backup resolves; cooldown visible
  (cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" ask demo "[SIMULATE_RATE_LIMIT] ping" >/dev/null 2>&1 || true)
  STATUS_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" status)"
  expect_contains "mock_primary" "$STATUS_OUT"
  expect_contains "rate_limited" "$STATUS_OUT"
  echo "PASS failover: role fallback & cooldown visible in status"

  # f. AF-05: budget guard fails fast on an oversized prompt
  if (cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" ask mock_backup "$(printf 'x%.0s' {1..500})" >/dev/null 2>&1); then
    echo "FAIL AF-05: oversized prompt was not rejected" >&2
    exit 1
  fi
  BUDGET_ERR="$(cd "$tmp_root/proj_a" && OCTOMESH_DEFAULT_CALLER=vibe "$OCTO_BIN" ask mock_backup "$(printf 'x%.0s' {1..500})" 2>&1 || true)"
  expect_contains "budget_exceeded" "$BUDGET_ERR"
  FAILED_TASKS="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" tasks --status failed)"
  expect_contains "budget_exceeded" "$FAILED_TASKS"
  (cd "$tmp_root/proj_a" && "$OCTO_BIN" budget --peer vibe >/dev/null)
  echo "PASS AF-05: budget guard + task failed + budget report"

  # g. AF-05: claim lease command succeeds (no queued tasks -> null claim)
  NEXT_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" next --peer codex)"
  expect_contains '"claimed": null' "$NEXT_OUT"
  echo "PASS AF-05: octo next claim command"

  # h. AF-06: connector permissions (gitlab not in allowlist)
  CONNECTORS_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" connectors)"
  expect_contains '"github"' "$CONNECTORS_OUT"
  if (cd "$tmp_root/proj_a" && "$OCTO_BIN" connector call gitlab '{"jsonrpc":"2.0","id":1,"method":"ping"}' >/dev/null 2>&1); then
    echo "FAIL AF-06: gitlab not allowed for proj_a but call succeeded" >&2
    exit 1
  fi
  DENY_OUT="$(cd "$tmp_root/proj_a" && "$OCTO_BIN" connector call gitlab '{"jsonrpc":"2.0","id":1,"method":"ping"}' 2>/dev/null || true)"
  expect_contains "connector_not_allowed" "$DENY_OUT"
  echo "PASS AF-06: connector gateway permission denial"
}

run_real() {
  if ! command -v tmux >/dev/null 2>&1; then
    echo "FAIL real: tmux is required for --real mode" >&2
    exit 1
  fi
  for sess in vibe codex agy; do
    if ! tmux has-session -t "$sess" 2>/dev/null; then
      tmux new-session -d -s "$sess"
    fi
  done
  echo "PASS real: tmux sessions (vibe, codex, agy) ready; see docs/ACCEPTANCE.md for manual sign-off"
}

case "$MODE" in
  --mock-only)
    run_mock_only
    ;;
  --real)
    run_real
    ;;
  *)
    echo "Usage: $0 [--mock-only|--real]" >&2
    exit 2
    ;;
esac
