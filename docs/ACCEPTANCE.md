# AF-08 — Manual acceptance checklist (real clients, several terminals)

Run each item in a separate real terminal window with the corresponding CLI
(Claude CLI, Codex CLI, Mistral Vibe, Antigravity/other) after `octo install`.
Automated CI coverage: `bash scripts/e2e_mesh.sh --mock-only` (job `e2e-mock`).

- [ ] **AF-01 (install & doctor)**: `octo install` installs `~/.local/bin/octo` and
  `~/.local/share/octomesh/adapters/`; from an unrelated directory `octo doctor`
  reports all OK (exit 0) and `octo ask demo "ping"` works with only the exported
  env vars (`PATH`, `OCTOMESH_CONFIG`, `OCTOMESH_DB`).
- [ ] **AF-02 (mailbox & project isolation)**: messages sent in project A never
  appear in project B's `octo inbox`; `octo inbox --ack <id>` sets `ack_at` only on
  the caller's own row; `octo projects` lists per-project unread counts.
- [ ] **AF-03 (presence auto-registration)**: starting a client with
  `OCTOMESH_DEFAULT_CALLER=codex` registers a LIVE row in `octo presence` /
  `mesh_status`; stale rows disappear after `presence_ttl_secs`.
- [ ] **AF-04 (window delivery & ack)**: `octo ask <window_peer> "..."` types
  `[Octomesh Delegation | task=<id> | ...]` into the live window; `octo tasks
  --peer <window_peer>` shows `delivered` on both sides; the receiver runs
  `octo ack <id>` -> `accepted`; a reply file ending with `<<OCTO_DONE>>` -> `done`.
- [ ] **AF-05 (coordination, crash recovery, budget)**: `octo next --peer X` claims
  atomically with a lease; killing a claiming process requeues after the lease;
  a peer left `busy` recovers automatically after `busy_ttl_secs`; an oversized
  prompt fails fast with `budget_exceeded` and marks the task `failed`;
  `octo budget --peer X` shows limit/used/remaining.
- [ ] **AF-06/07 (connector gateway & share)**: a project with
  `allowed_connectors = ["github"]` can call `github` and gets
  `connector_not_allowed` for `drive`; `octo connectors` lists registry entries
  (binary present, env key NAMES only); `octo connectors --audit` shows the audit
  trail; `octo share report.pdf --via drive --to agy --dry-run` touches nothing,
  and a real run posts a mailbox message with the link.

## Sign-off table

| Client            | Version | Terminal      | Result (PASS/FAIL) | Notes |
| ----------------- | ------- | ------------- | ------------------ | ----- |
| Codex CLI         |         |               |                    |       |
| Mistral Vibe CLI  |         |               |                    |       |
| Antigravity/other |         |               |                    |       |
| Claude CLI        |         |               |                    |       |
