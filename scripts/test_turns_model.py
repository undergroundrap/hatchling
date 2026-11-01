"""The scripted turns model's answers (WO-026): how many earlier exchanges
it was given, and what the owner said in them.

Run: python3 -m unittest discover -s scripts -p 'test_turns_model.py' -v
"""
import unittest

import turns_model


def system(content):
    return {"role": "system", "content": content}


def user(content):
    return {"role": "user", "content": content}


def assistant(content):
    return {"role": "assistant", "content": content}


class TurnsModelAnswers(unittest.TestCase):
    def test_a_first_message_has_no_earlier_turns(self):
        messages = [system("rules"), user("Remember the word heron.")]
        self.assertEqual(turns_model.answer(messages)["content"], "Turns before this: 0.")

    def test_each_earlier_exchange_is_counted_with_the_owners_words(self):
        messages = [
            system("rules"),
            user("one"), assistant("Turns before this: 0."),
            user("two"), assistant("Turns before this: 1. Earlier you said: one"),
            user("three"),
        ]
        self.assertEqual(
            turns_model.answer(messages)["content"],
            "Turns before this: 2. Earlier you said: one | two",
        )

    def test_messages_after_the_request_and_tool_calls_are_not_exchanges(self):
        call = turns_model.call("idea_list", {})
        messages = [system("rules"), user("now"), user("recalled memory"), call,
                    {"role": "tool", "content": "[]"}]
        self.assertEqual(turns_model.earlier(messages), [])


if __name__ == "__main__":
    unittest.main()
