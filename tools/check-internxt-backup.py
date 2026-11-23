"""Loopback acceptance test for the OctoMesh rclone backup command."""

import argparse
import os
import socket
import subprocess
import time
from pathlib import Path
from tempfile import TemporaryDirectory


def wait_until(check, *, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for the expected exchange state")


def test_exchange(octo, root):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    raw = root / "ciphertext"
    raw.mkdir()
    config = root / "rclone.conf"
    env = {**os.environ, "RCLONE_CONFIG": str(config)}

    def invoke(args, cwd=root, *, check=True):
        return subprocess.run(
            args,
            cwd=cwd,
            env=env,
            check=check,
            capture_output=True,
            text=True,
            timeout=30,
        )

    password = invoke(["rclone", "obscure", "disposable-loopback-test"]).stdout.strip()
    config.write_text(
        f"[dav]\ntype = webdav\nurl = http://127.0.0.1:{port}/\nvendor = other\n"
        f"\n[secure]\ntype = crypt\nremote = dav:Encrypted\npassword = {password}\n"
        "filename_encryption = standard\ndirectory_name_encryption = true\n"
    )
    config.chmod(0o600)
    for host in ("pc", "vps"):
        cwd = root / host
        cwd.mkdir()
        invoke([octo, "init"], cwd)
        invoke([octo, "workspace", "init"], cwd)

    def args(host, remote="secure:OctoMesh"):
        return [
            octo,
            "workspace",
            "backup",
            "--remote",
            remote,
            "--project",
            "p_123",
            "--machine",
            host,
            "--agent",
            "codex",
            "--session",
            "r01",
            "--settle-secs",
            "0",
        ]

    workspace = Path(".octomesh/workspace")
    pc, vps = (root / host / workspace for host in ("pc", "vps"))
    offline = pc / "files" / "offline.txt"
    offline.write_bytes(b"staged before deletion")
    assert invoke(args("pc"), root / "pc", check=False).returncode != 0
    offline.unlink()
    draft = pc / "files" / "report.txt"
    draft.write_bytes(b"PC version 1")
    assert invoke(args("pc"), root / "pc", check=False).returncode != 0
    draft.write_bytes(b"PC version 2")
    (pc / "files" / ".hidden").write_bytes(b"excluded")
    repository = pc / "files" / "Repozytoria" / "spatial-cad"
    repository.mkdir(parents=True)
    (repository / "geometry.txt").write_bytes(b"repository output")
    (vps / "files" / "report.txt").write_bytes(b"VPS version")

    server_args = ["rclone", "serve", "webdav", str(raw), "--addr", f"127.0.0.1:{port}"]
    with subprocess.Popen(
        server_args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ) as server:
        try:

            def is_ready():
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        return True
                except OSError:
                    return False

            wait_until(is_ready)
            assert (
                invoke(
                    args("pc", "dav:unencrypted"), root / "pc", check=False
                ).returncode
                != 0
            )
            assert not (raw / "unencrypted").exists()
            invoke(args("pc"), root / "pc")
            invoke(args("vps"), root / "vps")
            invoke(args("pc"), root / "pc")
            vps_inbox = vps / "inbox" / "p_123" / "pc"
            pc_inbox = pc / "inbox" / "p_123" / "vps"
            assert {p.read_bytes() for p in vps_inbox.rglob("report.txt")} == {
                b"PC version 1",
                b"PC version 2",
            }
            assert [p.read_bytes() for p in pc_inbox.rglob("report.txt")] == [
                b"VPS version"
            ]
            assert [p.read_bytes() for p in vps_inbox.rglob("offline.txt")] == [
                b"staged before deletion"
            ]
            assert [p.read_bytes() for p in vps_inbox.rglob("geometry.txt")] == [
                b"repository output"
            ]
            assert draft.read_bytes() == b"PC version 2"
            assert not list(vps_inbox.rglob(".hidden"))
            assert (vps / "files" / "report.txt").read_bytes() == b"VPS version"

            with subprocess.Popen(
                args("pc") + ["--watch", "--interval-secs", "1"],
                cwd=root / "pc",
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            ) as watcher:
                try:
                    (pc / "files" / "automatic.txt").write_bytes(b"captured by watcher")

                    def pulled_automatic_file():
                        invoke(args("vps"), root / "vps", check=False)
                        return list(vps_inbox.rglob("automatic.txt"))

                    wait_until(pulled_automatic_file)
                    assert (
                        next(vps_inbox.rglob("automatic.txt")).read_bytes()
                        == b"captured by watcher"
                    )
                finally:
                    watcher.terminate()
                    watcher.wait(timeout=10)

            cloud_args = [
                "rclone",
                "lsf",
                "secure:OctoMesh/Projekty/p_123",
                "--recursive",
                "--files-only",
            ]
            cloud_files = sorted(invoke(cloud_args).stdout.splitlines())
            assert len(cloud_files) == 12
            assert any(
                path.startswith(
                    "Repozytoria/spatial-cad/Maszyny/pc/Agenty/codex/Sesje/r01/"
                )
                for path in cloud_files
            )
            for file in raw.rglob("*"):
                if file.is_file():
                    assert file.name not in {"payload", "manifest.json", "report.txt"}
                    with file.open("rb") as stream:
                        assert stream.read(8) == b"RCLONE\x00\x00"
            invoke(args("pc"), root / "pc")
            invoke(args("vps"), root / "vps")
            assert sorted(invoke(cloud_args).stdout.splitlines()) == cloud_files
        finally:
            server.terminate()
            server.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--octo", type=Path, default=Path("target/debug/octo"))
    args = parser.parse_args()
    with TemporaryDirectory(prefix="octo-internxt-loopback-") as directory:
        test_exchange(args.octo.resolve(strict=True), Path(directory))
    print(
        "Encrypted loopback exchange, queue, conflicts, repository route and watch mode passed."
    )


if __name__ == "__main__":
    main()
