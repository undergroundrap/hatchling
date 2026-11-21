"""Tests for packaging/release-provenance.sh, the check of who built a
release's files (#556).

Each test sources the helper in `sh`, as scripts/install.sh does, with
stand-ins for `gh` and `runuser` on a PATH of its own: `gh` answers as the
test sets it (signed in or not, attested or not) and records what it's
asked, and `runuser` records who it runs as and runs the rest. Nothing
reaches GitHub.

    python3 -m unittest discover -s scripts -p 'test_provenance.py' -v
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HELPER = ROOT / "packaging" / "release-provenance.sh"
TOOLS = ["sh", "tail", "tr", "printf", "env", "cat"]


def stand_in(path: Path, body: str) -> None:
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


@unittest.skipUnless(os.uname().sysname == "Linux", "the installer's helpers are run by Linux's sh")
class ProvenanceTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp(prefix="hatchling-provenance-test."))
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        for tool in TOOLS:
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
        self.log = self.dir / "log"
        self.release = self.dir / "release"
        self.release.mkdir()
        (self.release / "SHA256SUMS").write_text("0" * 64 + "  hatchling-broker-x86_64-unknown-linux-gnu\n")
        stand_in(
            self.bin / "runuser",
            f'echo "runuser $*" >> {self.log}\n'
            'while [ "$1" != -- ]; do shift; done; shift\n'
            'exec "$@"\n',
        )

    def gh(self, signed_in: bool, attested: bool) -> None:
        stand_in(
            self.bin / "gh",
            f'echo "gh HOME=$HOME $*" >> {self.log}\n'
            'case "$1" in\n'
            f'  auth) exit {0 if signed_in else 1} ;;\n'
            '  attestation)\n'
            + ('    echo "Verification succeeded!"; exit 0 ;;\n' if attested else
               '    echo "Error: no attestations found for subject" >&2; exit 1 ;;\n')
            + 'esac\nexit 2\n',
        )

    def run_helper(self) -> subprocess.CompletedProcess[str]:
        script = (
            'say() { echo "== $*"; }\n'
            'fail() { echo "install.sh: $*" >&2; exit 1; }\n'
            f'. "{HELPER}"\n'
            "release_provenance\n"
            'echo "went on"\n'
        )
        return subprocess.run(
            ["sh", "-c", script],
            env={
                "PATH": str(self.bin),
                "FROM": str(self.release),
                "OWNER_NAME": "owner",
                "OWNER_HOME": "/home/owner",
            },
            capture_output=True,
            text=True,
            timeout=30,
        )

    def recorded(self) -> str:
        return self.log.read_text() if self.log.exists() else ""

    def test_attested_by_the_release_workflow_goes_on_saying_so(self) -> None:
        self.gh(signed_in=True, attested=True)
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("SHA256SUMS is attested by this project's release workflow", done.stdout)
        self.assertIn("went on", done.stdout)
        asked = self.recorded()
        self.assertIn(
            f"attestation verify {self.release}/SHA256SUMS --repo undergroundrap/hatchling "
            "--signer-workflow undergroundrap/hatchling/.github/workflows/release.yml",
            asked,
        )

    def test_not_attested_stops_the_install_saying_why(self) -> None:
        self.gh(signed_in=True, attested=False)
        done = self.run_helper()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("isn't attested by this project's release workflow", done.stderr)
        self.assertIn("nothing was installed", done.stderr)
        self.assertIn("no attestations found", done.stderr, "gh's own words are said")
        self.assertNotIn("went on", done.stdout)

    def test_gh_runs_as_the_owner_in_their_home(self) -> None:
        self.gh(signed_in=True, attested=True)
        self.run_helper()
        asked = self.recorded().splitlines()
        runs = [line for line in asked if line.startswith("runuser ")]
        ghs = [line for line in asked if line.startswith("gh ")]
        self.assertEqual(len(runs), 2, asked)
        self.assertEqual(len(ghs), 2, asked)
        for line in runs:
            self.assertTrue(line.startswith("runuser -u owner -- env HOME=/home/owner "), line)
        for line in ghs:
            self.assertTrue(line.startswith("gh HOME=/home/owner "), line)

    def test_signed_out_goes_on_saying_it_couldnt_check(self) -> None:
        self.gh(signed_in=False, attested=True)
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("isn't checked", done.stdout)
        self.assertIn("gh auth login", done.stdout)
        self.assertIn("went on", done.stdout)
        self.assertNotIn("attestation", self.recorded(), "verified with nobody signed in")

    def test_without_gh_it_goes_on_saying_how_to_check(self) -> None:
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("no gh here, so who built these files isn't checked", done.stdout)
        self.assertIn("gh attestation verify SHA256SUMS --repo undergroundrap/hatchling", done.stdout)
        self.assertIn("went on", done.stdout)
        self.assertEqual(self.recorded(), "")


if __name__ == "__main__":
    unittest.main()
