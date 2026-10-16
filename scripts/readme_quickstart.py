"""Run the README quickstart in a disposable root PID namespace.

CI invokes this with unshare --mount --pid --fork --kill-child --mount-proc.
The documented installation and account creation need a disposable machine.
With --app-python, it also opens the address the README's app section
prints in a browser (scripts/app_end_to_end.py, run by that interpreter).
"""

import argparse
import ast
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shlex
import subprocess
import time

# The sections whose shell blocks run: the quickstart up to its first
# subsection, and the app's.
QUICKSTART = "## Quickstart"
APP = "### Using the app"
APP_ADDRESS = re.compile(r"^Hatchling's UI: open (http://127\.0\.0\.1:\d+/#hatchling-ui=[0-9a-f]+)$", re.M)
APP_ADDRESS_WAIT_SECS = 30


@dataclass(frozen=True)
class Block:
    line: int
    commands: str


def extract(text):
    """Return executable fences in Quickstart, before its first subsection,
    and in the app's subsection."""
    blocks = []
    active = False
    seen = False
    fence = None
    language = None
    lines = []
    start = 0
    for number, line in enumerate(text.splitlines(keepends=True), 1):
        if fence is not None:
            if re.fullmatch(re.escape(fence[0]) + "{" + str(len(fence)) + r",}\s*", line.strip()):
                if active and language in {"bash", "sh", "shell"}:
                    commands = "".join(lines)
                    if not commands.strip():
                        raise ValueError("Quickstart contains an empty shell block")
                    blocks.append(Block(start, commands))
                fence = None
            else:
                lines.append(line)
            continue
        if re.match(r"^#{1,3} ", line):
            active = line.rstrip() in (QUICKSTART, APP) and (seen or line.rstrip() == QUICKSTART)
            seen = seen or line.rstrip() == QUICKSTART
            continue
        match = re.fullmatch(r"(`{3,}|~{3,})([\w-]*)\s*", line.strip())
        if match:
            fence, language = match.groups()
            start = number + 1
            lines = []
    if fence is not None:
        raise ValueError("README has an unclosed code fence")
    if not seen or not blocks:
        raise ValueError("README has no executable Quickstart")
    return blocks


def expected_answer(path):
    """Read the stand-in's constant without importing or starting its server."""
    for node in ast.parse(path.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "ANSWER" for target in node.targets
        ):
            answer = ast.literal_eval(node.value)
            if isinstance(answer, str) and answer and "\n" not in answer:
                return answer
    raise ValueError("Stand-in model has no single-line ANSWER constant")


def app_address(read_outputs, wait=APP_ADDRESS_WAIT_SECS):
    """The address the app's host printed, waiting for it: it runs in the
    background, so its line can land after its block has ended."""
    deadline = time.monotonic() + wait
    while True:
        for output in read_outputs():
            match = APP_ADDRESS.search(output)
            if match:
                return match.group(1)
        if time.monotonic() >= deadline:
            raise ValueError("The app's host printed no address")
        time.sleep(0.2)


def require_answer(outputs, answer):
    if not any("agent: " + answer in output.splitlines() for output in outputs):
        raise ValueError("Quickstart did not deliver the stand-in agent's answer")


def script_for(blocks, artifacts):
    """Keep exports, working directory and background services in one shell."""
    script = ["#!/bin/bash\nset -Eeuo pipefail\n"]
    # Paths are supplied by us, but quote them for spaces and apostrophes too.
    for index, block in enumerate(blocks, 1):
        script.append(f"\n# README line {block.line}\n{{\n{block.commands}\n}}")
        script.append(" > " + shlex.quote(str(artifacts / f"block-{index}.stdout")))
        script.append(" 2> " + shlex.quote(str(artifacts / f"block-{index}.stderr")) + "\n")
    return "".join(script)


def git(repo, *args):
    return subprocess.check_output(["git", "-c", f"safe.directory={repo}",
                                    "-C", str(repo), *args], text=True).strip()


def run(repo, artifacts, work, timeout, app_python=None):
    if os.geteuid() != 0 or os.getpid() != 1:
        raise ValueError("Run as root in a disposable PID namespace (see module usage)")
    if git(repo, "status", "--porcelain"):
        raise ValueError("Quickstart must test a clean committed checkout")
    if not work.is_dir() or any(work.iterdir()):
        raise ValueError("Quickstart requires an empty private work directory")
    commit = git(repo, "rev-parse", "HEAD")
    tree = git(repo, "rev-parse", "HEAD^{tree}")
    blocks = extract((repo / "README.md").read_text(encoding="utf-8"))
    answer = expected_answer(repo / "docs/examples/stand-in-model.py")
    artifacts.mkdir(parents=True, exist_ok=False)
    (artifacts / "README.md").write_bytes((repo / "README.md").read_bytes())
    (artifacts / "source.txt").write_text(f"commit {commit}\ntree {tree}\n", encoding="utf-8")
    script = artifacts / "extracted.sh"
    script.write_text(script_for(blocks, artifacts), encoding="utf-8")
    print(f"Executing {len(blocks)} README blocks from {commit}; logs: {artifacts}", flush=True)
    # The outer namespace launcher owns cleanup. It removes this directory
    # only after the PID namespace and every service/cell have ended.
    snapshot = work / "snapshot.git"
    config = work / "gitconfig"
    subprocess.run(["git", "config", "--file", str(config), "safe.directory", str(repo)], check=True)
    environment = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(config),
                       PYTHONUNBUFFERED="1")
    subprocess.run(["git", "clone", "--bare", "--no-local", str(repo), str(snapshot)],
                   env=environment, check=True)
    # Pin the documented clone URL to this checkout, including PR merge refs.
    subprocess.run(["git", "-C", str(snapshot), "update-ref", "--no-deref", "HEAD", commit], check=True)
    subprocess.run(["git", "config", "--file", str(config),
                    f"url.file://{snapshot}.insteadOf", "https://github.com/undergroundrap/hatchling"], check=True)
    clone_parent = work / "work"
    clone_parent.mkdir()
    # Killing PID 1 when the runner exits removes all descendants, including
    # cells which start new sessions. No host-wide pkill or process search.
    process = subprocess.Popen(["bash", str(script)], cwd=clone_parent, env=environment)
    try:
        status = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise ValueError(f"Quickstart exceeded its {timeout}-second deadline") from error
    if status:
        raise ValueError(f"An extracted README command failed (exit {status})")
    clone = clone_parent / "hatchling"
    if git(clone, "rev-parse", "HEAD") != commit or git(clone, "rev-parse", "HEAD^{tree}") != tree:
        raise ValueError("README cloned a different source tree")
    if git(clone, "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("README modified the cloned source before testing it")
    def read_outputs():
        return [(artifacts / f"block-{index}.stdout").read_text(encoding="utf-8")
                for index in range(1, len(blocks) + 1)]
    require_answer(read_outputs(), answer)
    print("agent: " + answer, flush=True)
    if app_python is not None:
        # The address carries the UI token: hand it over, never print it.
        address = app_address(read_outputs)
        subprocess.run([str(app_python), str(repo / "scripts/app_end_to_end.py"), address],
                       cwd=repo / "scripts", env=environment, check=True, timeout=300)
    else:
        print("app in a browser: not checked (no --app-python)", flush=True)
    print("README quickstart: passed", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("work", type=Path)
    parser.add_argument("--timeout", type=int, default=1500)
    parser.add_argument("--app-python", type=Path,
                        help="a Python with Playwright, to open the app in a browser")
    args = parser.parse_args()
    try:
        if args.timeout <= 0:
            raise ValueError("Timeout must be positive")
        run(args.repo.resolve(), args.artifacts.resolve(), args.work.resolve(), args.timeout,
            args.app_python)
    except (ValueError, OSError, subprocess.CalledProcessError,
            subprocess.TimeoutExpired) as error:
        parser.exit(1, f"README quickstart: {error}\n")


if __name__ == "__main__":
    main()
