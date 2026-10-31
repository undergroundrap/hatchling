"""The scripted idea model's turns (WO-032): saves the idea, proposes it,
and answers with what the tools said.

Run: python3 -m unittest discover -s scripts -p 'test_idea_model.py' -v
"""
import json
import unittest

import idea_model


def user(content):
    return {"role": "user", "content": content}


def tool(result):
    return {"role": "tool", "content": json.dumps(result)}


def called(message):
    function = message["tool_calls"][0]["function"]
    return function["name"], json.loads(function["arguments"])


class IdeaModelTurns(unittest.TestCase):
    def test_it_saves_the_idea_then_proposes_it_then_says_so(self):
        messages = [user(idea_model.TASK)]
        self.assertEqual(called(idea_model.next_message(messages)), ("idea_capture", {"text": idea_model.IDEA}))
        messages.append(tool({"idea": "idea-1", "revision": 1}))
        self.assertEqual(
            called(idea_model.next_message(messages)),
            ("build_propose", {"idea": "idea-1", "kind": "page", "description": idea_model.DESCRIPTION}),
        )
        messages.append(tool({"proposed": "act-1", "idea": "idea-1", "revision": 1, "kind": "page"}))
        self.assertEqual(idea_model.next_message(messages), {"role": "assistant", "content": idea_model.PROPOSED})

    def test_a_refusal_is_answered_with_what_the_tool_said(self):
        refused = "the owner wasn't asked: too many requests are waiting for the owner"
        messages = [user(idea_model.TASK), {"role": "tool", "content": refused}]
        self.assertTrue(idea_model.next_message(messages)["content"].startswith(idea_model.NOT_PROPOSED))
        messages = [user(idea_model.TASK), tool({"idea": "idea-1", "revision": 1}),
                    {"role": "tool", "content": refused}]
        answer = idea_model.next_message(messages)["content"]
        self.assertEqual(answer, f"{idea_model.NOT_PROPOSED} {refused}")

    def test_an_earlier_tasks_results_are_not_this_ones(self):
        messages = [user("earlier"), tool({"idea": "idea-0"}), user(idea_model.TASK)]
        self.assertEqual(called(idea_model.next_message(messages))[0], "idea_capture")


if __name__ == "__main__":
    unittest.main()
