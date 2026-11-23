# `octomesh-cli` (`octo`)

A hybrid Peer-to-Peer (P2P) & automatic failover mesh bridge for native terminal AI agents (`Claude CLI`, `Codex CLI`, `Mistral Vibe CLI`, `Lumo`, `Antigravity`, and custom local CLIs).

`octomesh-cli` exposes a local Model Context Protocol (MCP) server over `stdio` backed by a local SQLite blackboard (`.octomesh/state.db`) and a decoupled Python adapter layer. Terminal agents can delegate tasks directly to peer agents or abstract roles (e.g., `coder`, `reviewer`). If a peer hits a rate limit, exhausts tokens, or times out, `octo` marks the peer on cooldown and transparently fails over to the next healthy peer in the configured role chain while preserving task context and `git diff`.

---

## Architecture

```
+-----------------------------------------------------------------------------+
|                        Terminal AI Agents (MCP Clients)                     |
|      [ Claude CLI ]       [ Codex CLI ]       [ Mistral Vibe CLI ]          |
+-------------+-------------------+-----------------------+-------------------+
              |                   |                       |
              +-------------------+-----------------------+
                                  | MCP over stdio (JSON-RPC 2.0)
                                  v
+-----------------------------------------------------------------------------+
|                       `octo mcp-serve` / `octo ask`                         |
|                                                                             |
|  +---------------------------+        +----------------------------------+  |
|  |     Anti-Loop Guard       |        |       SQLite Blackboard          |  |
|  | - Reject caller == target |        |       (.octomesh/state.db)       |  |
|  | - Enforce max_depth (<=2) |        | - peer_health (ready/cooldown)   |  |
|  +-------------+-------------+        | - task_history (audit trail)     |  |
|                |                      | - handoffs (session checkpoints) |  |
|                v                      +-----------------+----------------+  |
|  +------------------------------------------------------+----------------+  |
|  |                          Failover Router                              |  |
|  |  1. Resolve peer or role chain (e.g. reviewer -> [claude, vibe])      |  |
|  |  2. Filter out peers on active cooldown                               |  |
|  |  3. Spawn decoupled Python adapter subprocess (JSON stdin/stdout)     |  |
|  |  4. On RATE_LIMITED / TIMEOUT -> set cooldown & retry next peer       |  |
|  +-----------------------------------+-----------------------------------+  |
+--------------------------------------|--------------------------------------+
                                       | JSON stdin / stdout (no PyO3)
                                       v
+-----------------------------------------------------------------------------+
|                    Python Adapter Layer (`uv run ...`)                      |
|  +-----------+  +-----------+  +-----------+  +-----------+  +-----------+  +-----------+  |
|  | mock.py   |  | claude.py |  | vibe.py   |  | codex.py  |  | agy.py    |  | dbus.py   |  |
|  | (test)    |  | (claude)  |  | (vibe)    |  | (codex)   |  | (agy)     |  | (window)  |  |
|  +-----------+  +-----------+  +-----------+  +-----------+  +-----------+  +-----------+  |
+-----------------------------------------------------------------------------+
```

---

## Directory Tree

```text
octomesh-cli/
├── Cargo.toml
├── README.md
├── octomesh.toml.example
├── .github/
│   └── workflows/
│       └── ci.yml
├── crates/
│   ├── octo-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs
│   │       ├── db.rs
│   │       ├── protocol.rs
│   │       └── router.rs
│   └── octo-cli/
│       ├── Cargo.toml
│       ├── src/
│       │   └── main.rs
│       └── tests/
│           ├── failover_integration.rs
│           └── messaging_integration.rs
├── adapters/
│   ├── pyproject.toml
│   ├── octomesh_adapters/
│   │   ├── __init__.py
│   │   ├── base.py
│   │   ├── context.py
│   │   ├── runner.py
│   │   ├── mock.py
│   │   ├── claude.py
│   │   ├── vibe.py
│   │   ├── codex.py
│   │   ├── agy.py
│   │   └── dbus.py
│   └── tests/
│       └── test_adapters.py
├── templates/
│   └── PROTOCOL.md
└── examples/
    └── mcp.json
```

---

## Quickstart

### Prerequisites
- **Rust** (`cargo`, 1.75+)
- **Python** (`3.11+`) and [`uv`](https://docs.astral.sh/uv/)

### 1. Build & Initialize
```bash
# Build the `octo` binary
cargo build --release

# Initialize mesh configuration and protocol docs in your project
./target/release/octo init --sync-agents
```

### 2. Test Out-of-the-Box Failover with `mock` Peers
The generated `octomesh.toml` includes pre-configured `mock_primary` and `mock_backup` peers under the `demo` role so you can test automatic failover immediately without spending API tokens:

```bash
# Inspect peer states
./target/release/octo status

# Trigger a simulated rate-limit on mock_primary; octo automatically fails over to mock_backup
./target/release/octo ask demo "[SIMULATE_RATE_LIMIT] Review the authentication middleware" --include-diff --readonly

# Check updated cooldown state in SQLite blackboard
./target/release/octo status
```

### 3. Register with an MCP Client
Add `octo mcp-serve` to your agent's MCP configuration (see `examples/mcp.json`):
```json
{
  "mcpServers": {
    "octomesh": {
      "command": "octo",
      "args": ["mcp-serve"]
    }
  }
}
```

---

## Shared project files

Run `octo workspace init` in the project. Agents can find its shared files
through the `mesh_workspace` MCP tool. Use `octo workspace publish RELATIVE_FILE`
to copy a finished artifact to the export folder for Filen Desktop.
For encrypted two-way exchange between machines, use
`octo workspace backup --watch` with an Internxt-backed rclone crypt remote.
[Shared workspace](docs/shared-workspace.md) · [Internxt backup](docs/internxt-backup.md)

## `octomesh.toml` Example

```toml
max_depth = 2
default_cooldown_secs = 300
adapter_runner = ["uv", "run", "--project", "adapters", "python", "-m", "octomesh_adapters.runner"]

[roles]
coder = ["claude_primary", "vibe_backup", "codex_fallback"]
reviewer = ["vibe_backup", "claude_primary"]
demo = ["mock_primary", "mock_backup"]

[peers.claude_primary]
adapter = "claude"
command = "claude"
timeout_secs = 120
cooldown_secs = 300

[peers.vibe_backup]
adapter = "vibe"
command = "vibe"
timeout_secs = 120
cooldown_secs = 180

[peers.codex_fallback]
adapter = "codex"
command = "codex"
timeout_secs = 120
cooldown_secs = 180

[peers.mock_primary]
adapter = "mock"
timeout_secs = 5
cooldown_secs = 60

[peers.mock_backup]
adapter = "mock"
timeout_secs = 5
cooldown_secs = 60
```

---

---

## Terminal-to-terminal messaging (D-Bus windows + mailbox)

`octo` replaces ad-hoc scripts and email between terminals with two
token-effective channels over the shared blackboard (`.octomesh/state.db`, WAL —
safe to use from multiple open terminals at once):

1. Send messages with `octo send <peer|role|*>` or `mesh_send`. Role messages
   go to each member once. A `*` broadcast has independent read status for
   each peer. Read your inbox with `octo inbox` or `mesh_inbox`. Marking
   messages read affects only the returned page, and the unread count shows
   what remains. The mailbox assumes trusted local peers sharing the same
   project database.
2. Configure an explicit D-Bus peer for an already open session. KDE Konsole
   requires `allow_terminal_paste=true`, its security-sensitive D-Bus API
   enabled, and a session ready to accept a prompt.
   Its `sendText` method stages a single line for manual submission in the
   target terminal. A trusted custom D-Bus service can accept a multiline
   prompt and handle submission itself.

Set `reply_file` to a relative path to wait for the agent's answer with
`octo ask` or `mesh_ask`. Each dispatch prompts the agent to write to a unique
file under that path's parent directory and finish with a standalone
`<<OCTO_DONE>>` line. Completed and late replies stay on disk. Keep the reply
directory outside version control. Without
`reply_file`, the call returns after D-Bus acknowledges the transfer. If a
D-Bus attempt fails or times out, inspect the target session before retrying.
OctoMesh stops failover at that attempt because the prompt may already have
arrived.

```toml
[peers.agy_window]
adapter = "dbus"
command = "gdbus"   # or "busctl"
args = [
  "bus=org.kde.konsole",
  "path=/Sessions/2",
  "method=org.kde.konsole.Session.sendText",
  "allow_terminal_paste=true",
]
timeout_secs = 120
cooldown_secs = 60
```

```bash
# async mailbox
./target/release/octo send agy "Review PR #12, focus on auth" --author vibe
./target/release/octo send reviewer "second opinion needed" --author vibe   # -> each role member
./target/release/octo send '*' "Build ready for review" --author codex
./target/release/octo inbox --peer agy --mark-read

# stage a prompt for manual submission in the configured Konsole session
./target/release/octo ask agy_window "Review the auth middleware" --caller vibe --readonly
```

---

## Adding a New CLI Adapter

Because `octo-core` communicates with adapters strictly over JSON on `stdin`/`stdout`, adding a new terminal agent requires zero Rust changes:

1. Create `adapters/octomesh_adapters/my_agent.py` subclassing `BaseAdapter`:
   ```python
   from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode

   class MyAgentAdapter(BaseAdapter):
       name = "my_agent"

       def execute(self, request: AdapterRequest) -> AdapterResponse:
           # Invoke your local CLI tool via subprocess.run(...)
           # Map rate-limit stderr/exit codes to ErrorCode.RATE_LIMITED
           return AdapterResponse(
               status=ErrorCode.OK,
               output="Completed task",
               peer=request.peer,
               adapter=self.name,
           )
   ```
2. Register `"my_agent": MyAgentAdapter` in `adapters/octomesh_adapters/runner.py`.
3. Add `[peers.my_peer]` with `adapter = "my_agent"` to `octomesh.toml`.
