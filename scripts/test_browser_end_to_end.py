import json
import unittest

import browsing_model as model
import sign_in_site as site


def tool(result):
    return {'role': 'tool', 'content': result}


def shown(text, elements):
    return json.dumps({'url': model.BROWSER_URL, 'title': site.TITLE, 'text': text, 'truncated': False,
                       'elements': [{'id': id_, 'role': role, 'name': name} for id_, role, name in elements]})


FORM = shown('Sign in', [(1, 'textbox', 'Email'), (2, 'password', 'Password'), (3, 'button', 'Sign in')])
FILLED = shown('Sign in', [(4, 'textbox', 'Email: [hidden]'), (5, 'password', 'Password'), (6, 'button', 'Sign in')])


def asked(*results):
    return [{'role': 'user', 'content': model.SIGN_IN_TASK}, *map(tool, results)]


def called(message):
    call = message['tool_calls'][0]['function']
    return call['name'], json.loads(call['arguments'])


class SignInModelTests(unittest.TestCase):
    def test_it_opens_the_site_signs_in_sends_the_form_and_says_what_the_site_said(self):
        self.assertEqual(called(model.next_message(asked())),
                         ('browser_open', {'url': model.BROWSER_URL, 'reason': model.SIGN_IN_REASON}))
        self.assertEqual(called(model.next_message(asked(FORM))),
                         ('browser_sign_in', {'password_element': 2, 'username_element': 1}))
        self.assertEqual(called(model.next_message(asked(FORM, FILLED))),
                         ('browser_act', {'element': 6, 'action': 'click'}))
        done = shown(site.SIGNED_IN, [])
        self.assertEqual(model.next_message(asked(FORM, FILLED, done))['content'], model.SIGNED_IN)

    def test_it_says_why_when_it_cant(self):
        refused = 'the owner keeps no sign-in for this site'
        said = model.next_message(asked(FORM, refused))['content']
        self.assertTrue(said.startswith(model.NOT_SIGNED_IN) and refused in said, said)
        no_form = shown('Nothing here', [(1, 'link', 'Home')])
        self.assertIn('no sign-in form', model.next_message(asked(no_form))['content'])
        still = shown('Sign in', [])
        self.assertIn("didn't say", model.next_message(asked(FORM, FILLED, still))['content'])

    def test_a_tasks_steps_are_its_own_and_the_first_task_is_unchanged(self):
        # The chat's earlier task, and its tool results, are history.
        earlier = [{'role': 'user', 'content': 'Use the browser, then tell me what happened.'},
                   tool('{"handed_back": true}'), {'role': 'assistant', 'content': 'done'}]
        self.assertEqual(called(model.next_message(earlier + asked()))[0], 'browser_open')
        first = [{'role': 'user', 'content': 'Use the browser, then tell me what happened.'}]
        self.assertEqual(called(model.next_message(first)),
                         ('browser_open', {'url': model.BROWSER_URL, 'reason': model.REASON}))


class SiteTests(unittest.TestCase):
    def test_a_form_body_is_read_for_its_email_and_password_alone(self):
        self.assertEqual(site.sent(b'email=zoe%40a.example&password=p%26w+1&other=x'),
                         {'email': 'zoe@a.example', 'password': 'p&w 1'})
        self.assertEqual(site.sent(b'password='), {'email': None, 'password': ''})

    def test_the_model_looks_for_what_the_site_says(self):
        self.assertIn(model.SITE_SIGNED_IN, site.DONE)
        self.assertEqual(model.SITE_SIGNED_IN, site.SIGNED_IN)


if __name__ == '__main__':
    unittest.main()
