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

1. **Async mailbox (structured, local):** `octo send <peer|role|*> "message"`
   and `octo inbox [--all] [--mark-read]`, or the MCP tools `mesh_send` /
   `mesh_inbox` from any agent. Messages are short structured rows in the
   `messages` table (sender, recipient, body, optional `ref_task`, read state) —
   nothing is re-parsed from mail, a `*` target broadcasts to everyone; a role target is expanded
   to each member peer at send time.
2. **Live window push (D-Bus):** peers with `adapter = "dbus"` type the message
   into the *already open* terminal window over the session bus (KDE Konsole
   `org.kde.konsole.Session.sendText`, or any custom helper exposing a
   sendText-style method — configure `bus=`, `path=`, `method=` in the peer's
   `args`). Use `mesh_ask` / `octo ask` on such a peer for synchronous
   request/response: with `reply_file=` set, the target agent writes its answer
   to that file and ends it with a `<<OCTO_DONE>>` line. Without `reply_file`
   the call returns immediately after typing — ideal for one-line
   notifications ("unread message from X — run `octo inbox`"), so full message
   bodies never cross the terminal window and no context is re-sent.

```toml
[roles]
windows = ["agy_window", "vibe_window"]

[peers.agy_window]
adapter = "dbus"
command = "gdbus"   # or "busctl"
args = [
  "bus=org.kde.konsole",
  "path=/Sessions/2",
  "method=org.kde.konsole.Session.sendText",
  "reply_file=.octomesh/inbox/agy_window.md",
]
timeout_secs = 120
cooldown_secs = 60
```

```bash
# async mailbox
./target/release/octo send agy "Review PR #12, focus on auth" --author vibe
./target/release/octo send reviewer "second opinion needed" --author vibe   # -> each role member
./target/release/octo inbox --peer agy --mark-read

# synchronous ask into the live agy window (typed via D-Bus, reply via inbox file)
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
