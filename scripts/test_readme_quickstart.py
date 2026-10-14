"""Check extraction, actual shell execution and the answer failure guard."""

from pathlib import Path
import subprocess
import tempfile
import unittest

from readme_quickstart import expected_answer, extract, require_answer, script_for


class QuickstartTests(unittest.TestCase):
    def test_commands_change_with_the_readme_and_share_shell_state(self):
        text = """## Quickstart
```bash
export ANSWER='first version'
```
```text
agent: not executable
```
```bash
printf '%s\\n' "$ANSWER"
```
### Running without root
```bash
exit 99
```
"""
        with tempfile.TemporaryDirectory() as task_temp:
            artifacts = Path(task_temp) / "a space's logs"
            artifacts.mkdir()
            for answer in ("first version", "edited README"):
                script = script_for(extract(text.replace("first version", answer)), artifacts)
                subprocess.run(["bash", "-c", script], check=True)
                self.assertEqual((artifacts / "block-2.stdout").read_text(), answer + "\n")
                self.assertEqual((artifacts / "block-1.stdout").read_text(), "")

    def test_headings_inside_fences_do_not_change_the_section(self):
        text = """```text
## Quickstart
```bash
````
## Quickstart
~~~bash
## shell comment
echo expected
~~~~
## Next section
```bash
exit 99
```
"""
        blocks = extract(text)
        self.assertEqual([block.commands for block in blocks], ["## shell comment\necho expected\n"])

    def test_refuses_missing_empty_and_unclosed_input(self):
        inputs = ["# Other\n", "## Quickstart\ntext only\n",
                  "## Quickstart\n```bash\n \n```\n",
                  "## Quickstart\n```bash\necho partial\n"]
        for text in inputs:
            with self.subTest(text=text), self.assertRaises(ValueError):
                extract(text)

    def test_schedule_acknowledgment_and_near_matches_cannot_pass(self):
        answer = "A real answer."
        for output in ("", "scheduled for 14:07 UTC\n", "agent: A different answer.\n",
                       "log: agent: A real answer.\n", "agent: A real answer. extra\n"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                require_answer([output], answer)
        require_answer(["scheduled for 14:07 UTC\n", "agent: A real answer.\n"], answer)

    def test_reads_the_constant_without_importing_the_model_server(self):
        with tempfile.TemporaryDirectory() as task_temp:
            path = Path(task_temp) / "stand-in.py"
            path.write_text('ANSWER = ("Hello " "cell.")\nraise RuntimeError("must not import")\n')
            self.assertEqual(expected_answer(path), "Hello cell.")
            path.write_text('ANSWER = str("dynamic")\n')
            with self.assertRaises(ValueError):
                expected_answer(path)


if __name__ == "__main__":
    unittest.main()
