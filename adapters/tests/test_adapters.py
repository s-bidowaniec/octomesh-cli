from __future__ import annotations

import json
from subprocess import CompletedProcess
from unittest.mock import patch

from octomesh_adapters.base import ErrorCode
from octomesh_adapters.runner import ADAPTER_REGISTRY, dispatch


def test_all_four_cli_adapters_registered():
    for name in ("mock", "claude", "vibe", "codex", "agy"):
        assert name in ADAPTER_REGISTRY


def test_mock_adapter_ok_and_readonly_context():
    payload = json.dumps(
        {
            "peer": "mock_backup",
            "adapter": "mock",
            "prompt": "Check edge cases",
            "caller": "claude",
            "depth": 1,
            "include_diff": False,
            "readonly": True,
            "timeout_secs": 5,
        }
    )
    resp = dispatch(payload)
    assert resp.status == ErrorCode.OK
    assert resp.peer == "mock_backup"
    assert "readonly=True" in resp.output
    assert "Check edge cases" in resp.output


def test_mock_adapter_detects_simulated_rate_limit():
    payload = json.dumps(
        {
            "peer": "mock_primary",
            "adapter": "mock",
            "prompt": "[SIMULATE_RATE_LIMIT] Refactor parser",
            "caller": "human-cli",
            "depth": 0,
        }
    )
    resp = dispatch(payload)
    assert resp.status == ErrorCode.RATE_LIMITED
    assert resp.error_message is not None
    assert "429" in resp.error_message


def test_mock_adapter_detects_simulated_timeout():
    payload = json.dumps(
        {
            "peer": "mock_primary",
            "adapter": "mock",
            "prompt": "[SIMULATE_TIMEOUT] Long analysis",
            "caller": "human-cli",
            "depth": 0,
        }
    )
    resp = dispatch(payload)
    assert resp.status == ErrorCode.TIMEOUT


def test_unknown_adapter_returns_cli_not_found():
    payload = json.dumps(
        {
            "peer": "ghost_peer",
            "adapter": "nonexistent_cli",
            "prompt": "Hello",
        }
    )
    resp = dispatch(payload)
    assert resp.status == ErrorCode.CLI_NOT_FOUND


def test_claude_adapter_maps_weekly_limit_to_rate_limited():
    fake_proc = CompletedProcess(
        args=["claude"],
        returncode=1,
        stdout="You've hit your weekly limit · resets Nov 24, 2pm · out of usage credits",
        stderr="",
    )
    payload = json.dumps(
        {
            "peer": "claude",
            "adapter": "claude",
            "prompt": "Say hi",
            "caller": "human-cli",
            "depth": 0,
            "readonly": True,
        }
    )
    with patch("shutil.which", return_value="/usr/bin/claude"), patch(
        "subprocess.run", return_value=fake_proc
    ) as mock_run:
        resp = dispatch(payload)

    assert resp.status == ErrorCode.RATE_LIMITED
    called_cmd = mock_run.call_args.args[0]
    assert "--permission-mode" in called_cmd
    assert "plan" in called_cmd


def test_agy_adapter_builds_readonly_and_auto_commands():
    fake_proc = CompletedProcess(args=["agy"], returncode=0, stdout="OK from agy\n", stderr="")
    payload = json.dumps(
        {
            "peer": "agy",
            "adapter": "agy",
            "prompt": "Audit code",
            "caller": "codex",
            "depth": 1,
            "readonly": True,
            "timeout_secs": 45,
        }
    )
    with patch("shutil.which", return_value="/usr/bin/agy"), patch(
        "subprocess.run", return_value=fake_proc
    ) as mock_run:
        resp = dispatch(payload)

    assert resp.status == ErrorCode.OK
    assert resp.output == "OK from agy"
    called_cmd = mock_run.call_args.args[0]
    assert "--print" in called_cmd
    assert "--mode" in called_cmd
    assert "plan" in called_cmd
    assert "--print-timeout" in called_cmd
    assert "45s" in called_cmd


def test_vibe_adapter_builds_readonly_command():
    fake_proc = CompletedProcess(args=["vibe"], returncode=0, stdout="OK from vibe\n", stderr="")
    payload = json.dumps(
        {
            "peer": "vibe",
            "adapter": "vibe",
            "prompt": "Review diff",
            "caller": "agy",
            "depth": 1,
            "readonly": True,
        }
    )
    with patch("shutil.which", return_value="/usr/bin/vibe"), patch(
        "subprocess.run", return_value=fake_proc
    ) as mock_run:
        resp = dispatch(payload)

    assert resp.status == ErrorCode.OK
    assert resp.output == "OK from vibe"
    called_cmd = mock_run.call_args.args[0]
    assert "--agent" in called_cmd
    assert "ask" in called_cmd
    assert "--output" in called_cmd
    assert "text" in called_cmd


def test_codex_adapter_uses_ephemeral_and_readonly_sandbox():
    def fake_run(cmd, **kwargs):
        out_idx = cmd.index("-o") + 1
        with open(cmd[out_idx], "w", encoding="utf-8") as fh:
            fh.write("Clean codex answer")
        return CompletedProcess(args=cmd, returncode=0, stdout="verbose logs", stderr="")

    payload = json.dumps(
        {
            "peer": "codex",
            "adapter": "codex",
            "prompt": "Explain module",
            "caller": "vibe",
            "depth": 1,
            "readonly": True,
        }
    )
    with patch("shutil.which", return_value="/usr/bin/codex"), patch(
        "subprocess.run", side_effect=fake_run
    ) as mock_run:
        resp = dispatch(payload)

    assert resp.status == ErrorCode.OK
    assert resp.output == "Clean codex answer"
    called_cmd = mock_run.call_args.args[0]
    assert "--ephemeral" in called_cmd
    assert "--skip-git-repo-check" in called_cmd
    assert "-s" in called_cmd
    assert "read-only" in called_cmd
