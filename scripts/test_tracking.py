"""Tests for scripts/tracking.py: the work orders and pull requests held
to their issues (WO-042). No network: the GitHub side is given as data."""

import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import tracking  # noqa: E402

LINK = "https://github.com/undergroundrap/hatchling/issues"


def tree(files):
    """A repository root holding `files`, a map of path to text."""
    root = pathlib.Path(tempfile.mkdtemp(prefix="tracking-"))
    for path, text in files.items():
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(text, encoding="utf-8")
    return root


class WorkOrders(unittest.TestCase):
    def test_a_work_order_is_read_by_its_status_and_issue(self):
        root = tree({
            "docs/workorders/workorder-001-a.md": f"**Status:** DONE (2026-01-01)\n**Issue:** [#7]({LINK}/7)\n",
            "docs/workorders/workorder-002-b.md": "**Status:** QUEUED\n**Issue:** none\n",
            "docs/workorders/README.md": "| [001](workorder-001-a.md) |\n",
        })
        self.assertEqual(tracking.work_orders(root), [
            {"number": "001", "status": "DONE", "issue": 7},
            {"number": "002", "status": "QUEUED", "issue": None},
        ])


class Issues(unittest.TestCase):
    def order(self, status, issue=5):
        return {"number": "034", "status": status, "issue": issue}

    def test_a_closed_work_order_with_an_open_issue_is_named(self):
        self.assertEqual(
            tracking.issue_findings([self.order("CLOSED")], {5: {"state": "open", "milestone": "v1"}}),
            ["WO-034 is CLOSED but its issue #5 is still open"],
        )

    def test_an_open_work_order_with_a_closed_issue_is_named(self):
        self.assertEqual(
            tracking.issue_findings([self.order("IN-PROGRESS")], {5: {"state": "closed", "milestone": "v1"}}),
            ["WO-034 is IN-PROGRESS but its issue #5 is closed"],
        )

    def test_an_open_work_order_off_every_milestone_is_named(self):
        self.assertEqual(
            tracking.issue_findings([self.order("QUEUED")], {5: {"state": "open", "milestone": None}}),
            ["WO-034 is QUEUED and its issue #5 is on no milestone"],
        )

    def test_an_issue_that_isnt_there_is_named(self):
        self.assertEqual(
            tracking.issue_findings([self.order("QUEUED")], {}),
            ["WO-034's issue #5 isn't there"],
        )

    def test_work_orders_that_agree_with_their_issues_pass(self):
        orders = [self.order("DONE", 1), self.order("DROPPED", 2), self.order("QUEUED", 3), self.order("CLOSED", None)]
        issues = {
            1: {"state": "closed", "milestone": None},
            2: {"state": "closed", "milestone": "v1"},
            3: {"state": "open", "milestone": "v0.9.0"},
        }
        self.assertEqual(tracking.issue_findings(orders, issues), [])


class PullRequests(unittest.TestCase):
    def test_a_change_to_crates_with_no_reference_is_named(self):
        found = tracking.pr_findings("Makes it faster.", [], ["crates/x/src/lib.rs"])
        self.assertEqual(len(found), 1)
        self.assertIn("Part of #N", found[0])
        self.assertEqual(tracking.pr_findings(None, [], ["crates/x/src/lib.rs"]), found)

    def test_a_reference_or_the_trivial_label_passes(self):
        crates = ["crates/x/src/lib.rs", "docs/a.md"]
        for body in ["Closes #12.", "closes #12", "Fixes #3", "Resolves #9", "Part of #394 and #482."]:
            self.assertEqual(tracking.pr_findings(body, [], crates), [], body)
        self.assertEqual(tracking.pr_findings("", ["trivial"], crates), [])

    def test_a_number_that_isnt_an_issue_reference_doesnt_count(self):
        self.assertEqual(len(tracking.pr_findings("Part of the plan, see step 4.", [], ["crates/a"])), 1)
        self.assertEqual(len(tracking.pr_findings("Closes the gap (#).", [], ["crates/a"])), 1)

    def test_a_change_outside_crates_needs_nothing(self):
        self.assertEqual(tracking.pr_findings("", [], ["docs/a.md", "frontend/src/x.ts"]), [])


class Closing(unittest.TestCase):
    def test_the_issues_a_pull_request_closes_are_read_from_its_body(self):
        self.assertEqual(tracking.closing("Closes #487. Part of #394. fixes #12, Resolves #3"), {487, 12, 3})
        self.assertEqual(tracking.closing(None), set())
        self.assertEqual(tracking.closing("Part of #394"), set())


class Usage(unittest.TestCase):
    def test_a_wrong_command_line_is_a_usage_error(self):
        self.assertEqual(tracking.main(["tracking.py"]), 64)
        self.assertEqual(tracking.main(["tracking.py", "pr"]), 64)
        self.assertEqual(tracking.main(["tracking.py", "issues", "a", "b"]), 64)


if __name__ == "__main__":
    unittest.main()
