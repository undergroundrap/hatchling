#!/usr/bin/env python3
"""Holds the work orders and pull requests to their issues (WO-042).

What the repository says of itself is checked by `hatchling-dev audit`.
This checks what only GitHub knows, read-only:

  tracking.py issues [EVENT.json]
                               each work order's tracking issue: a closed
                               work order's is closed, an open one's is open
                               and on a milestone. Given a pull request's
                               event, an issue it closes counts as closed:
                               the pull request that closes a work order
                               can't close its issue before it merges.
  tracking.py pr EVENT.json    a pull request that changes crates/ says
                               which issue it closes or is part of, or is
                               labelled `trivial`

Run from the repository's root, with `gh` signed in (CI's own token is
enough). Exit status: 0 when nothing is wrong, 1 when something is, with
each finding on its own line.
"""

import json
import pathlib
import re
import subprocess
import sys

WORKORDERS = pathlib.Path("docs/workorders")
ISSUE = re.compile(r"^\*\*Issue:\*\*.*\(https://github\.com/[^/]+/[^/]+/issues/(\d+)\)", re.M)
STATUS = re.compile(r"^\*\*Status:\*\*\s*([A-Z-]+)", re.M)
REFERENCE = re.compile(r"\b(?:closes|fixes|resolves|part of)\s+#\d+", re.I)
CLOSED = {"CLOSED", "DONE", "DROPPED"}
CLOSING = re.compile(r"\b(?:closes|fixes|resolves)\s+#(\d+)", re.I)


def work_orders(root):
    """Each work order's number, status and tracking issue (None when it
    names none), from its file."""
    found = []
    for path in sorted((root / WORKORDERS).glob("workorder-*.md")):
        text = path.read_text(encoding="utf-8")
        status = STATUS.search(text)
        issue = ISSUE.search(text)
        found.append({
            "number": path.name[len("workorder-"):len("workorder-") + 3],
            "status": status.group(1) if status else "",
            "issue": int(issue.group(1)) if issue else None,
        })
    return found


def issue_findings(orders, issues):
    """What's wrong between `orders` (from `work_orders`) and `issues`,
    a map of issue number to {"state": "open"|"closed", "milestone": str|None}."""
    findings = []
    for order in orders:
        number = order["issue"]
        if number is None:
            continue
        issue = issues.get(number)
        name = f"WO-{order['number']}"
        if issue is None:
            findings.append(f"{name}'s issue #{number} isn't there")
        elif order["status"] in CLOSED and issue["state"] != "closed":
            findings.append(f"{name} is {order['status']} but its issue #{number} is still open")
        elif order["status"] not in CLOSED and issue["state"] == "closed":
            findings.append(f"{name} is {order['status']} but its issue #{number} is closed")
        elif order["status"] not in CLOSED and not issue.get("milestone"):
            findings.append(f"{name} is {order['status']} and its issue #{number} is on no milestone")
    return findings


def closing(body):
    """The issues a pull request's `body` says it closes."""
    return {int(number) for number in CLOSING.findall(body or "")}


def pr_findings(body, labels, files):
    """What's wrong with a pull request: its `body`, its `labels` and the
    `files` it changes."""
    if not any(path.startswith("crates/") for path in files):
        return []
    if "trivial" in labels or REFERENCE.search(body or ""):
        return []
    return [
        "this pull request changes crates/ and says neither `Closes #N` nor `Part of #N`: "
        "name its issue, or label it `trivial` (AGENTS.md, \"Work orders, issues and pull requests\")"
    ]


def gh(*args):
    """`gh api` on the repository's own path, as JSON."""
    out = subprocess.run(["gh", "api", *args], check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def gh_lines(*args):
    """`gh api` on the repository's own path, one line of `--jq` output each."""
    out = subprocess.run(["gh", "api", *args], check=True, capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line]


def main(argv):
    root = pathlib.Path(".")
    if argv[1:2] == ["issues"] and len(argv) in (2, 3):
        closes = set()
        if len(argv) == 3:
            event = json.loads(pathlib.Path(argv[2]).read_text(encoding="utf-8"))
            closes = closing(event.get("pull_request", {}).get("body"))
        orders = work_orders(root)
        issues = {}
        for number in sorted({o["issue"] for o in orders if o["issue"] is not None}):
            try:
                issue = gh(f"repos/{{owner}}/{{repo}}/issues/{number}")
            except subprocess.CalledProcessError:
                continue
            milestone = issue.get("milestone") or {}
            state = "closed" if number in closes else issue["state"]
            issues[number] = {"state": state, "milestone": milestone.get("title")}
        findings = issue_findings(orders, issues)
    elif argv[1:2] == ["pr"] and len(argv) == 3:
        event = json.loads(pathlib.Path(argv[2]).read_text(encoding="utf-8"))
        pr = event["pull_request"]
        labels = [label["name"] for label in pr.get("labels", [])]
        files = gh_lines("--paginate", "--jq", ".[].filename", f"repos/{{owner}}/{{repo}}/pulls/{pr['number']}/files")
        findings = pr_findings(pr.get("body"), labels, files)
    else:
        print(__doc__, file=sys.stderr)
        return 64
    for finding in findings:
        print(finding)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
