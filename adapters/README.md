# octomesh-adapters

Python adapter layer for `octomesh-cli` (`octo`). Each adapter wraps one terminal
AI CLI (Claude, Codex, Mistral Vibe, Antigravity, or the offline `mock`) as a
headless subprocess.

Protocol: the runner reads one JSON `AdapterPayload` from stdin and writes one
JSON `AdapterResponse` to stdout (newline-delimited, no other stdout output).

```bash
uv sync --dev        # install dev deps (pytest, ruff)
uv run pytest -v     # tests
uv run ruff check .  # lint
```

Register a new adapter in `octomesh_adapters/runner.py` (`ADAPTER_REGISTRY`).
See `../README.md` for the mesh architecture and `octomesh.toml` peer config.
