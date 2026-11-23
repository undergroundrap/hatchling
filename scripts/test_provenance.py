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
TOOLS = ["sh", "tail", "tr", "printf", "env", "cat", "sort", "head"]


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

    def run_helper(self, call: str = "release_provenance", **asked: str) -> subprocess.CompletedProcess[str]:
        script = (
            'say() { echo "== $*"; }\n'
            'fail() { echo "install.sh: $*" >&2; exit 1; }\n'
            f'. "{HELPER}"\n'
            f"{call}\n"
            'echo "went on"\n'
        )
        return subprocess.run(
            ["sh", "-c", script],
            env={
                "PATH": str(self.bin),
                "FROM": str(self.release),
                "OWNER_NAME": "owner",
                "OWNER_HOME": "/home/owner",
                **asked,
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

    def test_signed_out_stops_unless_asked_to_go_on_unchecked(self) -> None:
        self.gh(signed_in=False, attested=True)
        done = self.run_helper()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("isn't signed in here as owner", done.stderr)
        self.assertIn("can't be checked, and nothing was installed", done.stderr)
        self.assertIn("gh auth login", done.stderr)
        self.assertIn("pass --unverified", done.stderr)
        self.assertNotIn("went on", done.stdout)
        done = self.run_helper(PROVENANCE_UNVERIFIED="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("isn't checked: installing them anyway, as --unverified asks", done.stdout)
        self.assertIn("went on", done.stdout)
        self.assertNotIn("attestation", self.recorded(), "verified with nobody signed in")

    def test_without_gh_it_stops_unless_asked_saying_how_to_check(self) -> None:
        done = self.run_helper()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("no gh here, so who built these files can't be checked, and nothing was installed", done.stderr)
        self.assertIn("install gh and sign in", done.stderr)
        self.assertNotIn("went on", done.stdout)
        done = self.run_helper(PROVENANCE_UNVERIFIED="1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("no gh here, so who built these files isn't checked", done.stdout)
        self.assertIn("gh attestation verify SHA256SUMS --repo undergroundrap/hatchling", done.stdout)
        self.assertIn("went on", done.stdout)
        self.assertEqual(self.recorded(), "")

    def test_not_attested_stops_even_when_asked_to_go_on_unchecked(self) -> None:
        self.gh(signed_in=True, attested=False)
        done = self.run_helper(PROVENANCE_UNVERIFIED="1")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("isn't attested by this project's release workflow", done.stderr)
        self.assertNotIn("went on", done.stdout)

    def older(self, installed: str, new: str, **asked: str) -> subprocess.CompletedProcess[str]:
        return self.run_helper(f'refuse_older "{installed}" "{new}"', **asked)

    def test_a_release_older_than_the_one_installed_stops_unless_asked(self) -> None:
        for installed, new in [("0.10.0", "0.9.2"), ("0.10.1", "0.10.0"), ("1.0.0", "0.10.0"), ("0.10.0", "0.9.10")]:
            with self.subTest(installed=installed, new=new):
                done = self.older(installed, new)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(f"these files are Hatchling {new}, older than {installed}, installed", done.stderr)
                self.assertIn("pass --allow-older", done.stderr)
                self.assertNotIn("went on", done.stdout)
                done = self.older(installed, new, PROVENANCE_ALLOW_OLDER="1")
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertIn(f"installing Hatchling {new} over {installed}, which is newer, as --allow-older asks", done.stdout)

    def test_the_same_a_newer_or_a_first_install_goes_on(self) -> None:
        for installed, new in [("0.10.0", "0.10.0"), ("0.9.2", "0.10.0"), ("0.9.10", "0.10.0"),
                               ("0.10.0-rc.1", "0.10.0"), ("0.10.0", "0.10.0-rc.2"), ("", "0.10.0"), ("", "")]:
            with self.subTest(installed=installed, new=new):
                done = self.older(installed, new)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertIn("went on", done.stdout)
                self.assertNotIn("allow-older", done.stdout)

    def test_a_brokers_version_is_what_it_says_run_from_a_copy(self) -> None:
        broker = self.dir / "hatchling-broker-x86_64-unknown-linux-gnu"
        broker.write_text('#!/bin/sh\n[ "$1" = --version ] || exit 9\nprintf "hatchling-broker 0.9.2\\ncommit abc\\n"\n')
        broker.chmod(0o644)  # as a download leaves it: not runnable where it is
        bin_dir = self.dir / "runs"
        for tool in ["mkdir", "mktemp", "cp", "chmod", "rm", "sed"]:
            found = shutil.which(tool)
            if found and not (self.bin / tool).exists():
                (self.bin / tool).symlink_to(found)
        done = self.run_helper(f'v=$(broker_version "{broker}" "{bin_dir}"); echo "version [$v]"')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("version [0.9.2]", done.stdout)
        self.assertEqual(list(bin_dir.iterdir()), [], "its copy is removed")
        done = self.run_helper(f'v=$(broker_version "{self.dir}/none" "{bin_dir}"); echo "version [$v]"')
        self.assertIn("version []", done.stdout)
        broker.write_text("#!/bin/sh\necho something else\n")
        done = self.run_helper(f'v=$(broker_version "{broker}" "{bin_dir}"); echo "version [$v]"')
        self.assertIn("version []", done.stdout, "a program that doesn't say it is hatchling-broker has no version")

    def test_files_whose_version_cant_be_read_stop_unless_asked(self) -> None:
        done = self.older("0.10.0", "")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("the files' version can't be read", done.stderr)
        self.assertEqual(self.older("0.10.0", "", PROVENANCE_ALLOW_OLDER="1").returncode, 0)


if __name__ == "__main__":
    unittest.main()
