# Octomesh Agent Collaboration Protocol

You are connected to **Octomesh** (`octo`), a local Peer-to-Peer (P2P) & automatic failover bridge connecting native terminal AI agents (`claude`, `codex`, `vibe`, `agy`) via MCP.

## Available MCP Tools
1. `mesh_status` — Inspect available peers, failover role chains, active cooldowns, and recent handoffs.
2. `mesh_ask` — Delegate a subtask, second opinion, or code review to another peer (`claude`, `codex`, `vibe`, `agy`, a configured D-Bus window peer) or role (`coder`, `reviewer`, `fast`, `all`).
3. `mesh_handoff` — Record a structured checkpoint to `.octomesh/state.db` before ending your turn or hitting context limits.
4. `mesh_send` — Send an async structured message to a peer, role, or `*` (broadcast) — blackboard mailbox; preferred over email between terminals.
5. `mesh_inbox` — Read messages addressed to you (pass `mark_read: true` when done).

## Mandatory Rules for Agents

1. **Identify Yourself (`caller`) & Track Hop `depth`:**
   - Always pass your own peer/CLI name in `caller` (e.g., `"claude"`, `"codex"`, `"vibe"`, `"agy"`).
   - When initiating a delegation, set `depth = 1`. If you were invoked via `mesh_ask` at depth `N`, pass `depth = N + 1` on any downstream delegation.
   - Never exceed `max_depth` (default `2`).

2. **Never Call Yourself:**
   - Do not set `target` equal to your own `caller` identity. Prefer routing by **role** (e.g., `target: "reviewer"`) so `octo` can automatically skip you and fail over if a peer is rate-limited.

3. **Respect `readonly` Review Mode:**
   - When delegating a code review or audit, set `readonly: true` and `include_diff: true`.
   - If you receive a prompt with `readonly=True`, **do not modify, create, or delete files**; return findings and patch suggestions in your text response only.

4. **Always Record a `mesh_handoff` Before Stopping:**
   - Before terminating a multi-step session, switching terminals, or stopping due to token/context exhaustion, call `mesh_handoff` with a concise `task_title`, `summary` of completed work, and concrete `next_steps`.

5. **Use the Mailbox for Async Messages:**
   - For asynchronous peer communication use `mesh_send` (target: peer, role, or `*` for broadcast) and read with `mesh_inbox`. Keep bodies short and structured; set `ref_task` when the message relates to a `mesh_ask` task.

6. **Window Peers (D-Bus):**
   - Answer submitted prompts in your current conversation. If the prompt names a reply file, write your final answer there and finish with a line consisting solely of `<<OCTO_DONE>>`.
