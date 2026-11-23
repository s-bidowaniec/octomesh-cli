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


def _dbus_payload(tmp_path, command="gdbus", extra_args=None):
    args = [
        "bus=org.kde.konsole",
        "path=/Sessions/2",
        "method=org.kde.konsole.Session.sendText",
        "allow_terminal_paste=true",
    ]
    if extra_args:
        args.extend(extra_args)
    return json.dumps(
        {
            "peer": "agy_window",
            "adapter": "dbus",
            "prompt": "Review the auth middleware",
            "caller": "vibe",
            "depth": 1,
            "readonly": True,
            "timeout_secs": 10,
            "command": command,
            "args": args,
            "working_dir": str(tmp_path),
        }
    )


def test_dbus_adapter_registered():
    assert "dbus" in ADAPTER_REGISTRY


def test_dbus_adapter_requires_bus_path_method(tmp_path):
    payload = json.dumps(
        {
            "peer": "agy_window",
            "adapter": "dbus",
            "prompt": "hi",
            "args": ["bus=org.kde.konsole"],
            "working_dir": str(tmp_path),
        }
    )
    resp = dispatch(payload)
    assert resp.status == ErrorCode.EXEC_ERROR
    assert "bus=" in resp.error_message


def test_dbus_adapter_types_into_window_and_reads_reply_file(tmp_path):
    reply = tmp_path / "inbox" / "reply.md"
    reply.parent.mkdir()
    reply.write_text("previous reply", encoding="utf-8")
    paths = []

    def fake_run(cmd, **kwargs):
        dispatched_reply = next(
            directory / reply.name
            for directory in reply.parent.glob("reply-*")
            if str(directory / reply.name) in cmd[-1]
        )
        paths.append(dispatched_reply)
        dispatched_reply.write_text(
            "Answer quoting <<OCTO_DONE>> here\nAnswer from live agy window\n<<OCTO_DONE>>\n",
            encoding="utf-8",
        )
        return CompletedProcess(args=cmd, returncode=0, stdout="(true,)\n", stderr="")

    payload = _dbus_payload(tmp_path, extra_args=["reply_file=inbox/reply.md"])
    with patch("shutil.which", return_value="/usr/bin/gdbus"), patch(
        "subprocess.run", side_effect=fake_run
    ) as mock_run:
        resp = dispatch(payload)
        assert dispatch(payload).status == ErrorCode.OK

    assert resp.status == ErrorCode.OK
    assert "Answer from live agy window" in resp.output
    assert resp.output.count("<<OCTO_DONE>>") == 1
    assert paths[0] != paths[1]
    assert reply.read_text(encoding="utf-8") == "previous reply"
    called_cmd = mock_run.call_args.args[0]
    assert called_cmd[:2] == ["gdbus", "call"]
    assert "--session" in called_cmd
    assert "org.kde.konsole" in called_cmd
    assert "/Sessions/2" in called_cmd
    assert "org.kde.konsole.Session.sendText" in called_cmd
    typed = called_cmd[-1]
    assert typed.startswith("'") and typed.endswith("'")
    assert "Review the auth middleware" in typed
    assert "readonly=True" in typed


def test_dbus_adapter_busctl_form_sends_raw_text(tmp_path):
    captured = {}

    def fake_run(cmd, **kwargs):
        captured["cmd"] = cmd
        return CompletedProcess(args=cmd, returncode=0, stdout="(true,)", stderr="")

    payload = json.loads(_dbus_payload(tmp_path, command="busctl"))
    payload["prompt"] = "Review the auth middleware\r\n\x1b[201~\x03Keep it staged"
    with patch("shutil.which", return_value="/usr/bin/busctl"), patch(
        "subprocess.run", side_effect=fake_run
    ):
        resp = dispatch(json.dumps(payload))

    assert resp.status == ErrorCode.OK
    assert "submit the prompt there" in resp.output
    cmd = captured["cmd"]
    assert cmd[:3] == ["busctl", "--user", "call"]
    assert cmd[3:7] == [
        "org.kde.konsole",
        "/Sessions/2",
        "org.kde.konsole.Session",
        "sendText",
    ]
    assert cmd[7] == "s"
    assert "Review the auth middleware" in cmd[8]
    assert all(ord(char) >= 32 for char in cmd[8])


def test_dbus_adapter_rejects_konsole_execution_and_unapproved_paste(tmp_path):
    for overrides in (
        ["method=org.kde.konsole.Session.runCommand"],
        ["allow_terminal_paste=false"],
    ):
        payload = _dbus_payload(tmp_path, extra_args=overrides)
        with patch("subprocess.run") as run:
            resp = dispatch(payload)
        assert resp.status == ErrorCode.EXEC_ERROR
        run.assert_not_called()


def test_dbus_adapter_rejects_external_reply_path(tmp_path):
    payload = _dbus_payload(tmp_path, extra_args=["reply_file=../outside.md"])
    with patch("shutil.which", return_value="/usr/bin/gdbus"), patch("subprocess.run") as run:
        resp = dispatch(payload)
    assert resp.status == ErrorCode.EXEC_ERROR
    assert "relative file path" in resp.error_message
    run.assert_not_called()


def test_dbus_adapter_maps_service_unknown_to_cli_not_found(tmp_path):
    fake_proc = CompletedProcess(
        args=["gdbus"],
        returncode=1,
        stdout="",
        stderr="org.freedesktop.DBus.Error.ServiceUnknown: "
        "The name org.kde.konsole was not provided by any .service files",
    )
    payload = _dbus_payload(tmp_path)
    with patch("shutil.which", return_value="/usr/bin/gdbus"), patch(
        "subprocess.run", return_value=fake_proc
    ):
        resp = dispatch(payload)
    assert resp.status == ErrorCode.CLI_NOT_FOUND


def test_dbus_adapter_missing_caller_binary_is_cli_not_found(tmp_path):
    payload = _dbus_payload(tmp_path, command="gdbus")
    with patch("shutil.which", return_value=None):
        resp = dispatch(payload)
    assert resp.status == ErrorCode.CLI_NOT_FOUND


def test_dbus_adapter_reply_timeout(tmp_path):
    payload = _dbus_payload(tmp_path, extra_args=["reply_file=inbox/r.md", "poll_secs=0.2"])
    payload = json.loads(payload)
    payload["timeout_secs"] = 1
    payload = json.dumps(payload)
    fake_proc = CompletedProcess(args=["gdbus"], returncode=0, stdout="(true,)", stderr="")
    with patch("shutil.which", return_value="/usr/bin/gdbus"), patch(
        "subprocess.run", return_value=fake_proc
    ):
        resp = dispatch(payload)
    assert resp.status == ErrorCode.TIMEOUT
    assert "<<OCTO_DONE>>" in resp.error_message
