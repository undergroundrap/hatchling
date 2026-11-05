"""Tests for scripts/get.sh, the one-command install.

Each test lays out a release as the release workflow does (the programs,
the app, the installer and get-hatchling.sh from
scripts/release-installer.sh, and SHA256SUMS), and runs the script from
it, on a PATH of its own: the tools it needs, and stand-ins for sudo,
ollama, id and nvidia-smi that record what they're asked. Nothing is
installed: the stand-in sudo records the installer it was handed.

It needs Linux with systemd running, as get.sh does: CI's runners, or a
systemd WSL distro.

    python3 -m unittest discover -s scripts -p 'test_get.py' -v
"""

from __future__ import annotations

import hashlib
import http.server
import io
import os
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = {"x86_64": "x86_64-unknown-linux-gnu", "aarch64": "aarch64-unknown-linux-gnu"}.get(
    os.uname().machine, "x86_64-unknown-linux-gnu"
)
TOOLS = [
    "sh", "curl", "sha256sum", "tar", "gzip", "grep", "sed", "cut", "head",
    "tr", "mktemp", "chmod", "rm", "mkdir", "cat", "uname", "dirname", "printf",
]
PROGRAMS = ["hatchling-broker", "hatchling-agent", "hatchling-host", "hatchling-browser"]


def stand_in(path: Path, body: str) -> None:
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


class Ollama(http.server.BaseHTTPRequestHandler):
    """Ollama's model list: the models in the class's `models`."""

    models: list[str] = []

    def do_GET(self) -> None:  # noqa: N802 (the handler's name)
        if self.path != "/api/tags":
            self.send_error(404)
            return
        names = ",".join(f'{{"name":"{name}"}}' for name in self.models)
        body = f'{{"models":[{names}]}}'.encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        pass


@unittest.skipUnless(
    os.path.isdir("/run/systemd/system") and os.uname().sysname == "Linux",
    "get.sh needs Linux with systemd running",
)
class GetTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp(prefix="hatchling-get-test."))
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.release = self.dir / "release"
        self.release.mkdir()
        for program in PROGRAMS:
            (self.release / f"{program}-{TARGET}").write_text(f"{program}\n")
        (self.release / "hatchling-ui.tar.gz").write_bytes(b"app")
        subprocess.run(
            ["sh", str(ROOT / "scripts/release-installer.sh"), str(self.release), "v0.0.0-test"],
            check=True,
        )
        self.sums()
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        for tool in TOOLS:
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
        self.log = self.dir / "log"
        stand_in(self.bin / "id", f'[ "$1" = -u ] && echo "${{FAKE_UID:-1000}}" && exit 0\nexec {shutil.which("id")} "$@"\n')
        stand_in(
            self.bin / "sudo",
            f'echo "sudo $*" >> {self.log}\n'
            f'[ -f "$2" ] && echo "installer there" >> {self.log}\n'
            f'[ -d "$(dirname "$2")/../packaging" ] && echo "packaging there" >> {self.log}\n'
            'echo "the installer ran"\n',
        )
        self.script = self.dir / "get-hatchling.sh"
        shutil.copy(self.release / "get-hatchling.sh", self.script)

    def sums(self) -> None:
        lines = []
        for file in sorted(self.release.iterdir()):
            if file.name != "SHA256SUMS":
                lines.append(f"{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.name}\n")
        (self.release / "SHA256SUMS").write_text("".join(lines))

    def ollama(self, models: list[str]) -> str:
        """An Ollama with `models` on this machine, and an ollama program; its address."""
        stand_in(self.bin / "ollama", f'echo "ollama $*" >> {self.log}\n')
        handler = type("Models", (Ollama,), {"models": models})
        server = http.server.HTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"127.0.0.1:{server.server_port}"

    def run_get(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        environment = {"PATH": str(self.bin), "HOME": str(self.dir), "TMPDIR": str(self.dir)}
        environment.update(env or {})
        return subprocess.run(
            ["sh", str(self.script), "--from-url", f"file://{self.release}", *args],
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
            start_new_session=True,  # no terminal to ask on
        )

    def recorded(self) -> str:
        return self.log.read_text() if self.log.exists() else ""

    def test_it_installs_the_checked_release_with_its_own_installer(self) -> None:
        self.assertIn("VERSION=v0.0.0-test\n", self.script.read_text())
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1", "--model", "stand-in")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("checking every file against the release's SHA256SUMS", done.stdout)
        self.assertIn("the installer ran", done.stdout)
        self.assertIn("sudo hatchling-broker use-hosted openrouter", done.stdout)
        sudo = self.recorded()
        self.assertRegex(
            sudo,
            r"sudo sh \S+/installer/scripts/install\.sh --from \S+ "
            r"--model-endpoint http://localhost:18089/v1 --model stand-in\n",
        )
        self.assertIn("installer there", sudo)
        self.assertIn("packaging there", sudo)
        leftovers = [p.name for p in self.dir.iterdir() if p.name.startswith("hatchling-get.")]
        self.assertEqual(leftovers, [], "its directory is removed")

    def test_a_file_that_differs_stops_it_before_anything_is_installed(self) -> None:
        (self.release / f"hatchling-broker-{TARGET}").write_text("something else\n")
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("a file differs from SHA256SUMS: nothing was installed", done.stderr)
        self.assertEqual(self.recorded(), "")

    def test_a_file_not_in_sha256sums_stops_it(self) -> None:
        sums = self.release / "SHA256SUMS"
        sums.write_text("".join(line for line in sums.read_text().splitlines(True) if "hatchling-host" not in line))
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn(f"hatchling-host-{TARGET} isn't in SHA256SUMS", done.stderr)
        self.assertEqual(self.recorded(), "")

    def installer_with(self, add: tarfile.TarInfo, data: bytes = b"") -> None:
        """The installer, with `add` added, and SHA256SUMS to match."""
        path = self.release / "hatchling-installer.tar.gz"
        with tarfile.open(path) as old:
            members = [(m, old.extractfile(m).read() if m.isfile() else None) for m in old.getmembers()]
        with tarfile.open(path, "w:gz") as new:
            for member, content in members:
                new.addfile(member, io.BytesIO(content) if content is not None else None)
            add.size = len(data)
            new.addfile(add, io.BytesIO(data))
        self.sums()

    def test_an_installer_holding_anything_else_is_refused(self) -> None:
        cases = {
            "evil.sh": "the installer holds evil.sh",
            "../outside": "outside its own directory",
            "/etc/outside": "outside its own directory",
        }
        for name, said in cases.items():
            with self.subTest(name=name):
                self.setUp()
                self.installer_with(tarfile.TarInfo(name), b"x")
                done = self.run_get("--model-endpoint", "http://localhost:18089/v1")
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(said, done.stderr)
                self.assertEqual(self.recorded(), "")

    def test_a_link_in_the_installer_is_refused(self) -> None:
        link = tarfile.TarInfo("packaging/systemd")
        link.type = tarfile.SYMTYPE
        link.linkname = "/etc/systemd/system"
        self.installer_with(link)
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("something other than files and directories", done.stderr)
        self.assertEqual(self.recorded(), "")

    def test_root_is_told_to_run_it_as_themselves(self) -> None:
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1", env={"FAKE_UID": "0"})
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("run it as yourself, not as root", done.stderr)

    def test_without_ollama_it_says_how_to_get_it_and_installs_nothing(self) -> None:
        done = self.run_get()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("Ollama isn't installed", done.stderr)
        self.assertIn("https://ollama.com/download", done.stderr)
        self.assertEqual(self.recorded(), "")

    def test_ollama_with_the_model_is_used_as_it_is(self) -> None:
        host = self.ollama(["qwen3.5:9b", "llama3.2:latest"])
        done = self.run_get(env={"OLLAMA_HOST": host})
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("Ollama has qwen3.5:9b", done.stdout)
        sudo = self.recorded()
        self.assertNotIn("ollama pull", sudo)
        self.assertIn(f"--model-endpoint http://{host}/v1 --model qwen3.5:9b", sudo)
        # A model's name with :latest is the same model.
        self.log.unlink()
        done = self.run_get("--model", "llama3.2", env={"OLLAMA_HOST": host})
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn("ollama pull", self.recorded())

    def test_a_missing_model_is_pulled_when_asked_to(self) -> None:
        host = self.ollama(["llama3.2:latest"])
        done = self.run_get("--yes", env={"OLLAMA_HOST": host})
        self.assertEqual(done.returncode, 0, done.stderr)
        sudo = self.recorded()
        self.assertIn("ollama pull qwen3.5:9b\n", sudo)
        self.assertLess(sudo.index("ollama pull"), sudo.index("sudo sh"))

    def test_with_nobody_to_ask_a_missing_model_isnt_pulled(self) -> None:
        host = self.ollama([])
        done = self.run_get(env={"OLLAMA_HOST": host})
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(self.recorded(), "")

    def test_ollama_not_answering_is_named(self) -> None:
        stand_in(self.bin / "ollama", "exit 0\n")
        done = self.run_get(env={"OLLAMA_HOST": "127.0.0.1:9"})
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("doesn't answer at http://127.0.0.1:9", done.stderr)

    def test_a_small_gpu_is_named(self) -> None:
        stand_in(self.bin / "nvidia-smi", "echo 4096\n")
        done = self.run_get("--model-endpoint", "http://localhost:18089/v1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn("4096 MiB", done.stdout, "a model server named isn't Ollama's")
        host = self.ollama(["qwen3.5:9b"])
        done = self.run_get(env={"OLLAMA_HOST": host})
        self.assertIn("This GPU has 4096 MiB", done.stdout)

    def test_a_hosted_model_needs_no_ollama_and_its_key_is_asked_by_the_installer(self) -> None:
        done = self.run_get("--hosted", "openrouter")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("using a hosted model, from openrouter", done.stdout)
        self.assertIn("sudo hatchling-broker use-local --model qwen3.5:9b", done.stdout)
        sudo = self.recorded()
        self.assertRegex(sudo, r"(?m)sudo sh \S+/install\.sh --from \S+ --hosted openrouter$")
        self.assertNotIn("--model", sudo)
        self.log.unlink()
        done = self.run_get("--hosted", "openrouter", "--model", "qwen/qwen3-coder:free")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertRegex(self.recorded(), r"(?m)--hosted openrouter --model qwen/qwen3-coder:free$")

    def test_without_ollama_a_hosted_model_is_offered(self) -> None:
        done = self.run_get()
        self.assertIn("sh get-hatchling.sh --hosted openrouter", done.stderr)

    def test_what_isnt_asked_for_properly_is_refused(self) -> None:
        for args, said in [
            (["--model", "a model"], "--model has a character"),
            (["--version", "main"], "--version is a release's tag"),
            (["--version", "v1;rm"], "--version is a release's tag"),
            (["--bogus"], "usage:"),
            (["--hosted", "elsewhere"], "--hosted names a provider Hatchling knows"),
            (["--hosted", "openrouter", "--model-endpoint", "http://localhost:1/v1"], "two models"),
        ]:
            with self.subTest(args=args):
                done = self.run_get(*args)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(said, done.stderr)
        environment = {"PATH": str(self.bin), "HOME": str(self.dir)}
        done = subprocess.run(
            ["sh", str(self.script), "--from-url", "http://example.invalid/"],
            env=environment, capture_output=True, text=True, timeout=30,
        )
        self.assertIn("--from-url is https://, or file://", done.stderr)


@unittest.skipUnless(os.uname().sysname == "Linux", "the release's tools are GNU's")
class ReleaseInstallerTest(unittest.TestCase):
    def test_the_installer_is_the_same_bytes_each_time_and_pinned_to_its_tag(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            for out in (first, second):
                subprocess.run(["sh", str(ROOT / "scripts/release-installer.sh"), out, "v9.9.9"], check=True)
            a = Path(first, "hatchling-installer.tar.gz").read_bytes()
            self.assertEqual(a, Path(second, "hatchling-installer.tar.gz").read_bytes())
            get = Path(first, "get-hatchling.sh").read_text()
            self.assertIn("\nVERSION=v9.9.9\n", get)
            self.assertNotIn("VERSION=latest", get)
            with tarfile.open(Path(first, "hatchling-installer.tar.gz")) as installer:
                names = installer.getnames()
                self.assertIn("scripts/install.sh", names)
                self.assertTrue(all(n == "scripts/install.sh" or n.startswith("packaging") for n in names), names)
                self.assertTrue(all(m.uid == 0 and m.mtime == 0 for m in installer.getmembers()))

    def test_a_tag_that_isnt_a_version_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as out:
            for tag in ["latest", "v1/2", "v1 2", ""]:
                done = subprocess.run(
                    ["sh", str(ROOT / "scripts/release-installer.sh"), out, tag], capture_output=True, text=True
                )
                self.assertNotEqual(done.returncode, 0, tag)


if __name__ == "__main__":
    unittest.main()
