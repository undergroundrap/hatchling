"""Real Chromium/DOM tests. Default: network-free standalone document.

Run with HATCHLING_BASE_URL=http://localhost:4173 for origin-backed HTTP mode.
Root/CI execution uses --no-sandbox; web security is not disabled.
A local evidence download is exercised. Hardware and external services are not.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import time
import unittest
import sys

from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[2]
SCREENSHOTS = ROOT / 'artifacts' / 'previews'
SCREENSHOTS.mkdir(parents=True, exist_ok=True)


from touchup_cases import TouchupCases
from release_cases import ReleaseCases
from navigation_cases import NavigationCases
from polish_cases import PolishCases
from conversation_cases import ConversationCases
from details_cases import DetailsCases
from refinement_cases import RefinementCases
from interaction_cases import InteractionCases


class FrontendTests(TouchupCases, ReleaseCases, NavigationCases, PolishCases, ConversationCases, InteractionCases, RefinementCases, DetailsCases, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.playwright = sync_playwright().start()
        executable = os.environ.get('CHROMIUM_PATH') or shutil.which('chromium') or shutil.which('chromium-browser')
        cls.browser = cls.playwright.chromium.launch(
            **({'executable_path': executable} if executable else {}),
            headless=True,
            args=['--no-sandbox'] if os.environ.get('CI') or os.geteuid() == 0 else [],
        )
        cls.html = (ROOT / 'dist' / 'hatchling-standalone.html').read_text()

    @classmethod
    def tearDownClass(cls):
        cls.browser.close()
        cls.playwright.stop()

    def setUp(self):
        self.context = self.browser.new_context(viewport={'width': 1919, 'height': 937}, color_scheme='dark', reduced_motion='reduce')
        self.page = self.context.new_page()
        self.page.set_default_timeout(5000)
        self.errors = []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.on('console', lambda message: self.errors.append(message.text) if message.type == 'error' else None)
        if os.environ.get('HATCHLING_BASE_URL'):
            self.page.goto(os.environ['HATCHLING_BASE_URL'], wait_until='domcontentloaded')
        else:
            self.page.set_content(self.html, wait_until='domcontentloaded')
        expect(self.page.get_by_role('textbox', name='Message', exact=True)).to_be_visible()

    def tearDown(self):
        errors = self.errors[:]
        self.context.close()
        self.assertEqual(errors, [], 'Browser console / page errors')

    def route(self, name):
        # Hash changes exercise the application's real router, including back/forward-compatible state.
        self.page.evaluate('(name) => { location.hash = name; }', name)
        self.page.wait_for_timeout(50)

    def open_settings(self):
        self.page.locator('#rail-more').click()
        self.page.locator('.rail-more').get_by_role('button', name='Settings', exact=True).click()

    def scenario(self, name):
        self.route('chat')
        self.page.keyboard.press('Control+Shift+D')
        self.page.locator(f'[data-scenario="{name}"]').click()

    def screenshot(self, name):
        self.page.screenshot(path=str(SCREENSHOTS / f'{name}.png'), animations='disabled')

    def assert_no_overflow(self):
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'Document overflows viewport')
        bad = self.page.evaluate("""() => [...document.querySelectorAll('main,.page,.composer,.settings-pane,.settings-nav,.dialog')].filter(el => {
            const rect = el.getBoundingClientRect();
            return rect.width > 0 && (rect.right > innerWidth + 1 || rect.left < -1 || el.scrollWidth > el.clientWidth + 2);
        }).map(el => `${el.className}: ${el.scrollWidth}/${el.clientWidth}`)""")
        self.assertEqual(bad, [], 'Content must reflow, not horizontally scroll')

    def test_01_shell_and_all_product_routes(self):
        expect(self.page.get_by_role('complementary', name='Agent status and activity')).to_be_visible()
        self.screenshot('01-chat-desktop')
        for name, heading in [('feed', 'Feed'), ('ideas', 'Ideas'), ('goals', 'Goals'), ('library', 'All artifacts'), ('browser', 'Browser'), ('memory', 'Memory'), ('schedule', 'Scheduled work'), ('notifications', 'Your daily digest')]:
            self.page.get_by_role('button', name=heading, exact=True).click() if name in ['feed', 'ideas', 'goals'] else self.route(name)
            expect(self.page.get_by_role('heading', name=heading, level=1, exact=True)).to_be_visible()
            self.assert_no_overflow()
            self.screenshot(f'{name}-desktop')

    def test_02_all_settings_sections_and_labeled_meters(self):
        self.open_settings()
        expect(self.page.locator('meter[aria-label]')).to_have_count(2)
        self.screenshot('settings-desktop')
        sections = self.page.get_by_role('navigation', name='Settings sections')
        for title in ['Connectors', 'Wallet', 'Secure store', 'Permissions', 'Messaging channels', 'Devices', 'Data controls', 'Help & support', 'Legal info', 'General']:
            sections.get_by_role('button', name=title, exact=True).click()
            if title == 'General':
                expect(self.page.locator('.settings-pane .account-card')).to_be_visible()
            else:
                expect(self.page.locator('.settings-pane').get_by_role('heading', name='Secure credentials store' if title == 'Secure store' else title, exact=True)).to_be_visible()
        self.page.keyboard.press('Escape')
        expect(self.page.get_by_role('dialog')).to_have_count(0)

    def test_03_side_chat_and_safe_message_rendering(self):
        self.page.get_by_role('button', name='New side chat', exact=True).click()
        self.page.get_by_label('Chat name', exact=True).fill('UI regression notes')
        self.page.get_by_role('button', name='Create side chat', exact=True).click()
        self.page.locator('#message-input').fill('<img src=x onerror=alert(1)>')
        self.page.get_by_role('button', name='Send message', exact=True).click()
        expect(self.page.locator('.user-bubble')).to_contain_text('<img src=x onerror=alert(1)>')
        self.assertEqual(self.page.locator('.user-bubble img').count(), 0)
        expect(self.page.get_by_role('button', name='UI regression notes', exact=True)).to_be_visible()

    def test_04_markdown_edit_and_fresh_share_approval(self):
        self.route('library')
        self.page.get_by_role('button', name='Open Hatchling improvement backlog.md', exact=True).click()
        self.page.get_by_role('button', name='Edit document', exact=True).click()
        self.page.get_by_label('Markdown content').fill('# A verified edit\n\nKept only in this local workspace.')
        self.page.get_by_role('button', name='Save document', exact=True).click()
        expect(self.page.locator('.editor-reading-column')).to_contain_text('A verified edit')
        self.page.get_by_role('button', name='Share', exact=True).click()
        expect(self.page.get_by_role('button', name='Approve snapshot', exact=True)).to_be_visible()
        self.page.get_by_role('button', name='Approve snapshot', exact=True).click()
        expect(self.page.get_by_label('Local snapshot preview link')).to_have_value(re.compile(r'#published/backlog-v2$'))

    def test_05_library_list_and_recoverable_trash(self):
        self.route('library')
        self.page.get_by_role('button', name='Switch to list view', exact=True).click()
        expect(self.page.locator('.artifact-list')).to_be_visible()
        self.page.locator('[data-artifact="backlog"] summary').click()
        self.page.get_by_role('button', name='Move to Trash', exact=True).click()
        self.page.get_by_role('dialog').get_by_role('button', name='Move to Trash', exact=True).click()
        expect(self.page.locator('[data-artifact="backlog"]')).to_have_count(0)
        self.route('library/trash')
        expect(self.page.locator('[data-artifact="backlog"]')).to_be_visible()
        self.page.locator('[data-artifact="backlog"] summary').click()
        self.page.get_by_role('button', name='Restore', exact=True).click()
        expect(self.page.locator('[data-artifact="backlog"]')).to_have_count(0)

    def test_06_goal_wizard_one_question_at_a_time(self):
        self.route('goals')
        self.page.get_by_label('Goal options', exact=True).click()
        self.page.get_by_role('button', name='New goal', exact=True).click()
        dialog = self.page.get_by_role('dialog')
        expect(dialog.locator('input,textarea')).to_have_count(0)
        dialog.get_by_role('button', name='Interests', exact=True).click()
        expect(dialog.locator('input,textarea')).to_have_count(1)
        dialog.get_by_label('Your answer', exact=True).fill('A book and a quiet hour')
        dialog.get_by_role('button', name='Continue', exact=True).click()
        expect(dialog.locator('input,textarea')).to_have_count(1)
        dialog.get_by_label('Your answer', exact=True).fill('Read the first chapter')
        dialog.get_by_role('button', name='Continue', exact=True).click()
        dialog.get_by_role('button', name='Create goal', exact=True).click()
        expect(self.page.get_by_role('button', name='A book and a quiet hour', exact=True)).to_be_visible()
        self.page.get_by_label('Goal options', exact=True).click()
        self.page.get_by_role('button', name='Show demonstration goals', exact=True).click()
        self.assertEqual(self.page.locator('[data-pulse-kind]').evaluate_all('(els) => new Set(els.map(el => el.dataset.pulseKind)).size'), 7)

    def test_07_idea_build_has_a_real_saved_result(self):
        self.route('ideas')
        self.page.locator('.idea-row .row-title-button').first.click()
        dialog = self.page.get_by_role('dialog')
        # The visible action is deliberately explicit about the scripted worker.
        dialog.get_by_role('button', name=re.compile('Activate|Let|Build|Do this|Make')).first.click()
        expect(self.page.locator('[data-idea-state="completed"]')).to_be_visible(timeout=7000)
        self.page.locator('[data-idea-state="completed"] .row-title-button').click()
        expect(self.page.get_by_role('dialog')).to_contain_text('saved')

    def test_08_feed_edit_generate_and_why(self):
        self.route('feed')
        self.page.get_by_label('Feed options', exact=True).click()
        self.page.get_by_role('button', name='Your feed prompt', exact=True).click()
        self.page.get_by_role('button', name='Edit', exact=True).click()
        self.page.get_by_label('What should hatchling write about?', exact=True).fill('Clear notes about making small, useful things.')
        self.page.get_by_role('button', name='Save prompt', exact=True).click()
        expect(self.page.locator('.feed-prompt')).to_contain_text('Clear notes about making small, useful things.')
        before = self.page.locator('.feed-post').count()
        self.page.get_by_role('button', name='Generate', exact=True).click()
        self.assertEqual(self.page.locator('.feed-post').count(), before + 1)
        self.page.locator('.feed-post details').first.locator('summary').click()
        self.page.locator('.feed-post details').first.get_by_role('button', name='Why did I see this?', exact=True).click()
        expect(self.page.get_by_role('dialog')).to_contain_text('Your saved feed direction:')

    def test_09_research_report_streams_live_and_preserves_child_lanes(self):
        self.scenario('research')
        self.page.get_by_role('button', name='Open evidence: Shape a quieter coast trip', exact=True).click()
        expect(self.page.locator('.child-lane')).to_be_visible()
        expect(self.page.locator('.action-record[data-status="failed"]')).to_be_visible(timeout=4000)
        self.page.locator('.action-record[data-status="failed"]').click()
        expect(self.page.locator('.activity-title-group > .pill')).to_have_text('Complete', timeout=6000)
        expect(self.page.locator('.action-record[data-status="failed"]')).to_have_attribute('aria-pressed', 'true')
        self.page.locator('.finish-entry').click()
        expect(self.page.locator('.finish-verdict')).to_contain_text('Finish verdict: success')
        self.screenshot('activity-evidence-desktop')

    def test_10_purchase_change_requires_another_approval(self):
        self.scenario('purchase')
        card = self.page.get_by_role('article', name='Purchase review')
        expect(card).to_contain_text('Visa .... 4242')
        card.get_by_role('button', name='Approve $79.44', exact=True).click()
        card.get_by_role('button', name='Simulate price +$5', exact=True).click()
        expect(card.get_by_role('button', name='Approve $84.44', exact=True)).to_be_visible()
        expect(card.get_by_role('button', name='Submit sample order once', exact=True)).to_have_count(0)
        self.screenshot('purchase-review-desktop')

    def test_11_unknown_payment_exposes_takeover_not_retry(self):
        self.scenario('unknown-payment')
        self.page.get_by_role('button', name='Approve $79.44', exact=True).click()
        self.page.get_by_role('button', name='Submit sample order once', exact=True).click()
        expect(self.page.locator('[data-payment-state="unknown"]')).to_be_visible(timeout=4000)
        expect(self.page.get_by_role('button', name='Take over and check', exact=True)).to_be_visible()
        expect(self.page.get_by_role('button', name='Submit sample order once', exact=True)).to_have_count(0)

    def test_12_challenge_permission_and_persistent_handoff(self):
        self.scenario('captcha')
        expect(self.page.get_by_role('button', name='Allow this challenge', exact=True)).to_be_visible()
        self.page.get_by_role('button', name='Allow this challenge', exact=True).click()
        expect(self.page.get_by_role('button', name='Allow this challenge', exact=True)).to_have_count(0)
        self.scenario('persistent-captcha')
        expect(self.page.get_by_role('button', name='Allow this challenge', exact=True)).to_have_count(0)
        self.page.get_by_role('button', name='Solve it myself', exact=True).click()
        self.page.get_by_role('button', name='Complete sample challenge yourself', exact=True).click()
        self.page.get_by_role('button', name='Hand control back', exact=True).click()
        expect(self.page.get_by_role('button', name='Take over', exact=True)).to_be_visible()

    def test_13_channel_five_states_and_read_only_conversation(self):
        self.scenario('channels')
        pane = self.page.locator('.settings-pane')
        for text in ['Re-check status', 'Try again later', 'Continue existing handoff', 'Connect securely']:
            expect(pane.get_by_role('button', name=text, exact=True)).to_be_visible()
        self.page.keyboard.press('Escape')
        self.route('chat/chat-whatsapp')
        expect(self.page.locator('#message-input')).to_have_count(0)
        expect(self.page.locator('.connector-conversation-banner')).to_contain_text('Read-only here')

    def test_14_choices_do_not_turn_free_form_questions_into_buttons(self):
        self.scenario('choices')
        expect(self.page.locator('.choice-options button')).to_have_count(3)
        self.page.get_by_role('button', name='The coast walk', exact=True).click()
        expect(self.page.get_by_role('button', name='The coast walk', exact=True)).to_be_disabled()
        self.assertEqual(self.page.get_by_role('button', name='What would make this feel like a good weekend for you?', exact=True).count(), 0)

    def test_15_keyboard_modal_focus_and_shortcuts(self):
        self.open_settings()
        for _ in range(24):
            self.page.keyboard.press('Tab')
            self.assertTrue(self.page.evaluate('Boolean(document.activeElement?.closest("dialog"))'))
        self.page.keyboard.press('Escape')
        self.page.keyboard.press('Control+k')
        expect(self.page.get_by_role('dialog', name='Search', exact=True)).to_be_visible()
        expect(self.page.get_by_role('combobox', name='Search workspace')).to_be_focused()
        self.page.keyboard.press('Alt+3')
        expect(self.page.get_by_role('heading', name='Feed', exact=True, level=1)).to_be_visible()

    def test_16_dark_by_default_system_and_explicit_light_mode(self):
        # Dark by default, even on a light system; System follows the system.
        self.page.emulate_media(color_scheme='light')
        self.assertEqual(self.page.locator('body').evaluate('(el) => getComputedStyle(el).backgroundColor'), 'rgb(24, 24, 24)')
        self.open_settings()
        expect(self.page.get_by_role('radio', name='Dark mode', exact=True)).to_be_checked()
        self.page.get_by_role('radio', name='System mode', exact=True).click()
        self.assertEqual(self.page.locator('body').evaluate('(el) => getComputedStyle(el).backgroundColor'), 'rgb(255, 255, 255)')
        self.page.get_by_role('radio', name='Dark mode', exact=True).click()
        self.assertEqual(self.page.locator('body').evaluate('(el) => getComputedStyle(el).backgroundColor'), 'rgb(24, 24, 24)')
        self.page.get_by_role('radio', name='Light mode', exact=True).click()
        self.page.keyboard.press('Escape')
        self.screenshot('chat-light-desktop')
        self.assertEqual(self.page.locator('.agent-portrait img').evaluate('(el) => getComputedStyle(el).filter'), 'none')

    def test_17_phone_reflow_all_main_pages_and_settings(self):
        self.page.set_viewport_size({'width': 390, 'height': 844})
        # Crossing into mobile closes desktop panels; the user can explicitly reopen them.
        expect(self.page.locator('.context-panel')).to_have_count(0)
        expect(self.page.locator('.navigation-panel')).to_have_count(0)
        for route in ['chat', 'feed', 'ideas', 'goals', 'library', 'browser', 'memory', 'schedule', 'notifications']:
            self.route(route)
            self.assert_no_overflow()
            if route in ['chat', 'feed', 'library']:
                self.screenshot(f'{route}-mobile')
        self.open_settings()
        self.assert_no_overflow()
        self.screenshot('settings-mobile')
        for section in ['wallet', 'permissions', 'channels', 'devices', 'data', 'general']:
            self.page.get_by_label('Settings section', exact=True).select_option(section)
            self.assert_no_overflow()

    def test_18_narrow_phone_reflows_purchase_and_tables(self):
        self.page.set_viewport_size({'width': 320, 'height': 760})
        expect(self.page.locator('.context-panel')).to_have_count(0)
        self.scenario('purchase')
        self.assert_no_overflow()
        self.route('library/system')
        self.assert_no_overflow()
        expect(self.page.get_by_role('heading', name='System files', exact=True, level=1)).to_be_visible()

    def test_19_context_tabs_are_local_and_preserve_draft_and_transcript_scroll(self):
        draft = self.page.get_by_role('textbox', name='Message', exact=True)
        draft.fill('Keep this unfinished thought')
        stream = self.page.locator('.chat-scroll')
        stream.evaluate('(el) => { el.scrollTop = 120; }')
        self.page.wait_for_timeout(30)
        before = stream.evaluate('(el) => el.scrollTop')
        original_hash = self.page.evaluate('location.hash')
        for label in ['Activity', 'Approvals', 'Schedule', 'Identity']:
            self.page.get_by_role('tab', name=label, exact=True).click()
            expect(self.page.get_by_role('tab', name=label, exact=True)).to_have_attribute('aria-selected', 'true')
            expect(draft).to_have_value('Keep this unfinished thought')
            self.assertAlmostEqual(stream.evaluate('(el) => el.scrollTop'), before, delta=1)
            self.assertEqual(self.page.evaluate('location.hash'), original_hash)
            expect(self.page.get_by_role('dialog')).to_have_count(0)

    def test_20_context_keyboard_uses_roving_tabs_and_wraps(self):
        self.page.get_by_role('tab', name='Identity', exact=True).focus()
        for key, expected in [('ArrowRight', 'Activity'), ('ArrowRight', 'Approvals'), ('End', 'Identity'), ('Home', 'Activity'), ('ArrowLeft', 'Identity')]:
            self.page.keyboard.press(key)
            target = self.page.get_by_role('tab', name=expected, exact=True)
            expect(target).to_be_focused()
            expect(target).to_have_attribute('aria-selected', 'true')
        self.assertEqual(self.page.locator('.context-tabs [tabindex="0"]').count(), 1)

    def test_21_approvals_expand_in_place_and_separate_live_permissions(self):
        self.page.get_by_role('tab', name='Approvals', exact=True).click()
        expect(self.page.get_by_role('heading', name='Approvals history', exact=True)).to_be_visible()
        records = self.page.locator('.approval-record')
        expect(records).to_have_count(5)
        records.first.locator('summary').click()
        expect(records.first).to_have_attribute('open', '')
        expect(records.first.locator('.approval-report')).to_contain_text('No download or network request occurred')
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        self.page.get_by_role('button', name='Permissions & active grants', exact=True).click()
        expect(self.page.locator('.settings-pane').get_by_role('heading', name='Permissions', exact=True)).to_be_visible()

    def test_22_identity_edit_and_cancel_do_not_open_settings(self):
        self.page.locator('.identity-card').get_by_role('button', name='Edit', exact=True).click()
        doc = self.page.locator('.identity-document-dock')
        expect(doc).to_be_visible()
        self.assertEqual(self.page.locator('dialog:modal').count(), 0)
        doc.get_by_label('Document options', exact=True).click()
        doc.get_by_role('button', name='Edit document', exact=True).click()
        field = doc.get_by_role('textbox', name='IDENTITY.md', exact=True)
        field.fill(field.input_value().replace('hatchling', 'Pip').replace('warm, direct, a little playful; co-engineer energy', 'Warm, concise, curious.'))
        doc.get_by_role('button', name='Save document', exact=True).click()
        expect(doc.locator('.editor-reading-column')).to_contain_text('Pip')
        doc.get_by_role('button', name='Close IDENTITY.md', exact=True).click()
        expect(self.page.locator('.agent-profile h2')).to_have_text('Pip')
        expect(self.page.locator('.identity-vibe p')).to_have_text('Warm, concise, curious.')
        self.page.locator('.identity-card').get_by_role('button', name='Edit', exact=True).click()
        doc.get_by_label('Document options', exact=True).click()
        doc.get_by_role('button', name='Edit document', exact=True).click()
        field.fill(field.input_value().replace('Pip', 'Discard this'))
        doc.get_by_role('button', name='Cancel edits', exact=True).click()
        doc.get_by_role('button', name='Close IDENTITY.md', exact=True).click()
        expect(self.page.locator('.agent-profile h2')).to_have_text('Pip')

    def test_23_identity_covers_open_real_editable_documents(self):
        for title in ['SOUL', 'MEMORY']:
            self.page.get_by_role('button', name=f'Open {title}', exact=True).click()
            dialog = self.page.get_by_role('dialog', name=f'{title}.md', exact=True)
            expect(dialog.locator('.editor-reading-column > .markdown')).to_be_visible()
            dialog.get_by_role('button', name='Edit document', exact=True).click()
            dialog.get_by_role('textbox', name=title, exact=True).fill(f'{title}: measured, local, and evidence-led.')
            dialog.get_by_role('button', name='Save document', exact=True).click()
            expect(dialog).to_contain_text('measured, local, and evidence-led')
            self.page.keyboard.press('Escape')
            self.page.get_by_role('button', name=f'Open {title}', exact=True).click()
            expect(self.page.get_by_role('dialog', name=f'{title}.md', exact=True)).to_contain_text('measured, local, and evidence-led')
            self.page.keyboard.press('Escape')

    def test_24_search_overlay_recents_query_keyboard_and_recent_visit(self):
        self.page.get_by_role('button', name='Search chats', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='Search', exact=True)
        expect(dialog.get_by_role('option')).to_have_count(6)
        expect(dialog.get_by_role('option').first).to_have_accessible_name('Organize a sample project')
        query = dialog.get_by_role('combobox', name='Search workspace')
        expect(query).to_be_focused()
        query.fill('Review the sample checklist')
        expect(dialog.get_by_role('option', name='Review the sample checklist', exact=True)).to_be_visible()
        query.press('Enter')
        expect(dialog).to_have_count(0)
        self.assertEqual(self.page.evaluate('location.hash'), '#chat/chat-1')
        self.page.keyboard.press('Control+k')
        expect(self.page.get_by_role('option').first).to_have_accessible_name('Review the sample checklist')
        self.page.get_by_role('combobox', name='Search workspace').fill('no such item 99z')
        expect(self.page.locator('.search-empty')).to_contain_text('No results')
        expect(self.page.get_by_role('option')).to_have_count(0)

    def test_25_search_escape_backdrop_focus_and_draft_are_preserved(self):
        draft = self.page.get_by_role('textbox', name='Message', exact=True)
        draft.fill('Do not lose this draft')
        self.page.get_by_role('button', name='Search chats', exact=True).click()
        query = self.page.get_by_role('combobox', name='Search workspace')
        for _ in range(3):
            self.page.keyboard.press('Tab')
            expect(query).to_be_focused()
        query.press('ArrowDown')
        expect(self.page.get_by_role('option').nth(1)).to_have_attribute('aria-selected', 'true')
        self.page.keyboard.press('Escape')
        expect(self.page.get_by_role('button', name='Search chats', exact=True)).to_be_focused()
        expect(draft).to_have_value('Do not lose this draft')
        self.page.keyboard.press('Control+k')
        self.page.mouse.click(500, 850)
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        expect(draft).to_have_value('Do not lose this draft')

    def test_26_schedule_pause_resume_and_exactly_one_sample_run(self):
        self.page.get_by_role('tab', name='Schedule', exact=True).click()
        expect(self.page.locator('.context-job')).to_have_count(7)
        row = self.page.get_by_role('button', name='Open schedule: Heartbeat', exact=True)
        row.click()
        self.page.get_by_role('dialog', name='Heartbeat').get_by_role('button', name='Pause', exact=True).click()
        expect(row).to_contain_text('Paused')
        row.click()
        self.page.get_by_role('dialog', name='Heartbeat').get_by_role('button', name='Resume', exact=True).click()
        expect(row).to_contain_text('Every 30 minutes')
        tour = self.page.get_by_role('button', name='Open schedule: Agentic feature tour', exact=True)
        tour.click()
        self.page.get_by_role('button', name='Run sample now', exact=True).click()
        self.page.get_by_role('button', name='Run sample', exact=True).click()
        expect(tour).to_contain_text('Paused')
        tour.click()
        expect(self.page.get_by_role('button', name='Run sample now', exact=True)).to_be_disabled()
        expect(self.page.get_by_role('dialog', name='Agentic feature tour')).to_contain_text('Completed its one authorized run')

    def test_27_schedule_edit_requires_fresh_recurrence_approval(self):
        self.page.get_by_role('tab', name='Schedule', exact=True).click()
        self.page.get_by_role('button', name='Open schedule: Heartbeat', exact=True).click()
        self.page.get_by_role('button', name='Edit schedule', exact=True).click()
        self.page.get_by_label('When · include time and timezone', exact=True).fill('Every 45 minutes')
        self.page.get_by_role('button', name='Save and approve this recurrence', exact=True).click()
        confirmation = self.page.get_by_role('dialog', name='Approve recurring work?', exact=True)
        expect(confirmation).to_contain_text('Every 45 minutes')
        confirmation.get_by_role('button', name='Cancel', exact=True).click()
        expect(self.page.get_by_role('dialog', name='Edit schedule', exact=True)).to_be_visible()
        self.page.get_by_role('button', name='Save and approve this recurrence', exact=True).click()
        self.page.get_by_role('button', name='Approve recurrence', exact=True).click()
        expect(self.page.get_by_role('button', name='Open schedule: Heartbeat', exact=True)).to_contain_text('Every 45 minutes')

    def test_28_schedule_create_and_confirmed_removal(self):
        self.page.get_by_role('tab', name='Schedule', exact=True).click()
        self.page.get_by_role('button', name='Add scheduled work', exact=True).click()
        self.page.get_by_label('What should happen?', exact=True).fill('Check the visual diff')
        self.page.get_by_label('When · include time and timezone', exact=True).fill('October 1, 2026 at 9 am UTC')
        self.page.get_by_role('button', name='Create one run only', exact=True).click()
        row = self.page.get_by_role('button', name='Open schedule: Check the visual diff', exact=True)
        expect(row).to_be_visible()
        row.click()
        self.page.get_by_role('button', name='Remove schedule', exact=True).click()
        self.page.get_by_role('dialog', name='Remove scheduled work?', exact=True).get_by_role('button', name='Cancel', exact=True).click()
        expect(self.page.get_by_role('dialog', name='Check the visual diff')).to_be_visible()
        self.page.get_by_role('button', name='Remove schedule', exact=True).click()
        self.page.get_by_role('dialog', name='Remove scheduled work?', exact=True).get_by_role('button', name='Remove schedule', exact=True).click()
        expect(row).to_have_count(0)

    def test_29_composer_reveals_send_shift_enter_and_ime_guard(self):
        draft = self.page.get_by_role('textbox', name='Message', exact=True)
        send = self.page.get_by_role('button', name='Send message', exact=True)
        expect(send).to_be_hidden()
        expect(self.page.get_by_role('button', name='Voice dictation preview', exact=True)).to_be_visible()
        draft.fill('A first line')
        expect(send).to_be_visible()
        draft.press('End'); draft.press('Shift+Enter'); draft.type('A second line')
        before = self.page.locator('.message-row').count()
        draft.dispatch_event('keydown', {'key': 'Enter', 'isComposing': True, 'bubbles': True})
        self.assertEqual(self.page.locator('.message-row').count(), before)
        expect(draft).to_have_value('A first line\nA second line')
        draft.press('Enter')
        expect(draft).to_have_value('')
        expect(self.page.locator('.user-bubble').last).to_contain_text('A second line')
        expect(send).to_be_hidden()

    def test_30_per_chat_drafts_and_scroll_restore(self):
        draft = self.page.get_by_role('textbox', name='Message', exact=True)
        draft.fill('A reference draft\nwith a second line')
        self.page.locator('.chat-scroll').evaluate('(el) => { el.scrollTop = 180; }')
        self.page.wait_for_timeout(30)
        self.page.get_by_role('button', name='Main chat', exact=True).click()
        expect(draft).to_have_value('')
        draft.fill('A separate main draft')
        self.page.get_by_role('button', name='Organize a sample project', exact=True).click()
        expect(draft).to_have_value('A reference draft\nwith a second line')
        self.assertAlmostEqual(self.page.locator('.chat-scroll').evaluate('(el) => el.scrollTop'), 180, delta=1)
        self.page.get_by_role('button', name='Main chat', exact=True).click()
        expect(draft).to_have_value('A separate main draft')

    def test_31_context_close_reopen_keeps_selected_tab(self):
        self.page.get_by_role('tab', name='Approvals', exact=True).click()
        self.page.get_by_role('button', name='Hide agent status', exact=True).click()
        expect(self.page.locator('.context-panel')).to_have_count(0)
        self.page.get_by_role('button', name='Show agent status', exact=True).click()
        expect(self.page.get_by_role('tab', name='Approvals', exact=True)).to_have_attribute('aria-selected', 'true')
        expect(self.page.locator('.approval-record')).to_have_count(5)

    def test_32_phone_context_tabs_search_and_tooltips_do_not_overflow(self):
        for width in [390, 320]:
            self.page.set_viewport_size({'width': width, 'height': 844})
            self.page.get_by_role('button', name='Show agent status', exact=True).click()
            for label in ['Identity', 'Schedule', 'Approvals', 'Activity']:
                self.page.get_by_role('tab', name=label, exact=True).click()
                self.assert_no_overflow()
                bounds = self.page.locator('.context-panel').bounding_box()
                self.assertLessEqual(bounds['x'] + bounds['width'], width + 1)
            self.page.get_by_role('button', name='Hide agent status', exact=True).click()
            self.page.keyboard.press('Control+k')
            self.assert_no_overflow()
            self.page.keyboard.press('Escape')
            self.page.get_by_role('button', name='Voice dictation preview', exact=True).focus()
            self.assert_no_overflow()

    def test_33_reference_geometry_and_search_overlay_bounds(self):
        self.page.set_viewport_size({'width': 1907, 'height': 934})
        self.page.evaluate('() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))')
        # The touch-up narrows the chrome while retaining Comfortable text sizing.
        expect(self.page.locator('html')).to_have_attribute('data-interface-size', 'comfortable')
        self.assertAlmostEqual(self.page.locator('.icon-rail').bounding_box()['width'], 64 * 1.08, delta=1)
        self.assertAlmostEqual(self.page.locator('.navigation-panel').bounding_box()['width'], 232 * 1.08, delta=1)
        composer = self.page.locator('.composer').bounding_box()
        self.assertGreaterEqual(composer['height'], 54 * 1.08 - 1)
        self.assertLessEqual(composer['y'] + composer['height'], 934)
        self.page.keyboard.press('Control+k')
        search = self.page.locator('.search-dialog').bounding_box()
        self.assertGreaterEqual(search['width'], 500)
        self.assertGreaterEqual(search['y'], 0)
        self.assertLessEqual(search['y']+search['height'], 934)
        self.assert_no_overflow()

    def test_34_feed_tooltip_and_more_menu_expose_secondary_areas(self):
        feed = self.page.get_by_role('button', name='Feed', exact=True)
        feed.hover()
        self.assertEqual(feed.evaluate('(el) => getComputedStyle(el, "::after").content'), '"Feed"')
        self.page.locator('#rail-more').click()
        menu = self.page.locator('.rail-more .menu-popover')
        expect(menu.locator('button')).to_have_count(4)
        for title in ['Keyboard shortcuts', 'Download apps', 'Report an issue', 'Settings']:
            expect(menu.get_by_role('button', name=title, exact=True)).to_be_visible()
        self.page.mouse.click(450, 850)
        expect(menu).to_be_hidden()

    def test_35_resizing_keeps_a_bottom_anchored_conversation_at_the_latest_message(self):
        stream = self.page.locator('.chat-scroll')
        self.assertLessEqual(stream.evaluate('(el) => el.scrollHeight - el.scrollTop - el.clientHeight'), 2)
        for width, height in [(1440, 900), (390, 844), (320, 760), (1907, 934)]:
            self.page.set_viewport_size({'width': width, 'height': height})
            self.page.wait_for_timeout(80)
            self.assertLessEqual(stream.evaluate('(el) => el.scrollHeight - el.scrollTop - el.clientHeight'), 2)

    def test_36_goal_reference_geometry_and_category_first_flow(self):
        self.page.set_viewport_size({'width': 1569, 'height': 931})
        self.route('goals')
        expect(self.page.locator('.goal-row')).to_have_count(2)
        expect(self.page.locator('.goal-category')).to_have_count(7)
        main = self.page.locator('.main-region').bounding_box()
        goals = self.page.locator('.goals-page').bounding_box()
        self.assertAlmostEqual(goals['x'] + goals['width'] / 2, main['x'] + main['width'] / 2, delta=1)
        self.page.get_by_role('button', name='Health', exact=True).click()
        expect(self.page.get_by_role('dialog').locator('input,textarea')).to_have_count(1)
        expect(self.page.get_by_role('dialog')).to_contain_text('What would you like to work toward?')
        self.page.keyboard.press('Escape')
        self.page.get_by_role('button', name='Something else', exact=True).click()
        self.page.get_by_label('Your answer', exact=True).fill('A custom direction')
        self.page.get_by_role('button', name='Continue', exact=True).click()
        self.page.get_by_label('Your answer', exact=True).fill('One useful local result')
        self.page.get_by_role('button', name='Continue', exact=True).click()
        self.page.get_by_role('button', name='Create goal', exact=True).click()
        expect(self.page.get_by_role('button', name='A custom direction', exact=True)).to_be_visible()

    def test_37_goal_close_cancellation_and_reopen(self):
        self.route('goals')
        title = 'Sample workshop materials'
        self.page.get_by_role('checkbox', name=f'Close {title}', exact=True).click()
        self.page.keyboard.press('Escape')
        expect(self.page.get_by_role('checkbox', name=f'Close {title}', exact=True)).not_to_be_checked()
        self.page.get_by_role('checkbox', name=f'Close {title}', exact=True).click()
        self.page.get_by_role('dialog').get_by_role('button', name='Close responsibility', exact=True).click()
        expect(self.page.get_by_role('button', name=title, exact=True)).to_have_count(0)
        self.page.get_by_label('Goal options', exact=True).click()
        self.page.get_by_role('button', name='Closed goals', exact=True).click()
        self.page.get_by_role('checkbox', name=f'Reopen {title}', exact=True).click()
        self.page.get_by_label('Goal options', exact=True).click()
        self.page.get_by_role('button', name='Active goals', exact=True).click()
        expect(self.page.get_by_role('button', name=title, exact=True)).to_be_visible()

    def test_38_ideas_grouped_reference_rows_and_scope(self):
        self.route('ideas')
        expect(self.page.locator('.idea-row')).to_have_count(7)
        expect(self.page.locator('.idea-section-title')).to_have_text('Productivity')
        self.page.locator('.idea-row .row-title-button').first.click()
        expect(self.page.get_by_role('dialog')).to_contain_text('Synthetic fact')
        self.page.get_by_role('button', name='Adjust scope', exact=True).click()
        self.page.get_by_label('What to include', exact=True).fill('Only prepare a local review outline.')
        self.page.get_by_role('button', name='Save scope', exact=True).click()
        expect(self.page.get_by_role('dialog')).to_contain_text('Only prepare a local review outline.')

    def test_39_feed_reaction_provenance_share_and_media(self):
        self.route('feed')
        expect(self.page.locator('.feed-prompt')).to_be_hidden()
        post = self.page.locator('[data-post="ref-feed-companion"]')
        post.get_by_role('button', name='React to post', exact=True).click()
        expect(post.get_by_role('button', name='Remove reaction', exact=True)).to_have_attribute('aria-pressed', 'true')
        post.get_by_role('button', name='sample notebook', exact=True).click()
        expect(self.page.get_by_role('dialog')).to_contain_text('not inferred from a user profile')
        self.page.keyboard.press('Escape')
        post.locator('summary').click()
        post.get_by_role('button', name='Share', exact=True).click()
        expect(self.page.get_by_label('Post text', exact=True)).to_have_value(re.compile('Synthetic editorial example, not news.'))
        self.page.keyboard.press('Escape')
        post.get_by_role('button', name=re.compile('Enlarge image:')).click()
        expect(self.page.get_by_role('dialog').locator('img')).to_be_visible()
        self.page.keyboard.press('Escape')
        post.get_by_role('button', name='Discuss', exact=True).click()
        expect(self.page.get_by_role('textbox', name='Message', exact=True)).to_have_value(re.compile('synthetic editorial example'))

    def test_40_system_file_hierarchy_search_sort_and_protection(self):
        self.route('library/system')
        expect(self.page.locator('.system-table tbody tr')).to_have_count(16)
        self.page.get_by_label('Search library', exact=True).fill('WoRk')
        expect(self.page.locator('.system-table tbody tr')).to_have_count(1)
        self.page.get_by_label('Search library', exact=True).fill('')
        self.page.get_by_role('button', name='Sort files by name descending', exact=True).click()
        expect(self.page.locator('.system-entry').first).to_have_text('workspace')
        self.page.get_by_role('button', name='Open workspace folder', exact=True).click()
        expect(self.page.get_by_role('heading', level=1)).to_have_text('workspace')
        self.page.get_by_role('button', name='Open documents folder', exact=True).click()
        expect(self.page.locator('.system-table')).to_contain_text('Hatchling improvement backlog.md')
        self.page.get_by_role('button', name='Hatchling improvement backlog.md', exact=True).click()
        expect(self.page.get_by_role('dialog')).to_be_visible()
        self.page.keyboard.press('Escape')
        self.page.get_by_role('navigation', name='Folder path').get_by_role('button', name='System files', exact=True).click()
        self.page.get_by_role('button', name='Open agents folder', exact=True).click()
        expect(self.page.locator('.system-scope')).to_contain_text('Protected internal metadata')
        self.page.get_by_role('button', name='Workspace overview', exact=True).click()
        expect(self.page.get_by_role('dialog')).to_contain_text('not connected')

    def test_41_back_and_forward_restore_system_folder(self):
        self.route('library/system')
        self.page.get_by_role('button', name='Open workspace folder', exact=True).click()
        self.page.get_by_role('button', name='Open documents folder', exact=True).click()
        self.page.evaluate('history.back()')
        expect(self.page.get_by_role('heading', level=1)).to_have_text('workspace')
        self.page.evaluate('history.forward()')
        expect(self.page.get_by_role('heading', level=1)).to_have_text('documents')

    def test_42_new_surfaces_reflow_at_320_and_keep_media_uninverted(self):
        self.page.set_viewport_size({'width': 320, 'height': 760})
        for route in ['goals', 'ideas', 'feed', 'library/system']:
            self.route(route)
            self.assert_no_overflow()
        self.open_settings()
        self.page.get_by_role('radio', name='Light mode', exact=True).click()
        self.page.keyboard.press('Escape')
        self.route('feed')
        self.assert_no_overflow()
        self.assertEqual(self.page.locator('.feed-media img').first.evaluate('(el) => getComputedStyle(el).filter'), 'none')

    def test_43_original_icon_motion_is_single_play_and_reduced_motion_aware(self):
        self.page.emulate_media(reduced_motion='no-preference')
        self.page.locator('#rail-goals').hover()
        glyph = self.page.locator('#rail-goals svg')
        expect(glyph).to_have_class(re.compile('icon-playing'))
        expect(glyph).to_have_attribute('data-renderer', 'lottie-svg')
        before = glyph.get_attribute('data-frame')
        self.page.wait_for_timeout(120)
        self.assertNotEqual(glyph.get_attribute('data-frame'), before)
        self.page.wait_for_timeout(1300)
        expect(glyph).not_to_have_class(re.compile('icon-playing'))
        self.page.emulate_media(reduced_motion='reduce')
        self.page.locator('#rail-ideas').hover()
        expect(self.page.locator('#rail-ideas svg')).not_to_have_class(re.compile('icon-playing'))

    def test_44_goal_navigation_toggle_opens_and_dismisses_navigation(self):
        self.route('goals')
        expect(self.page.locator('.navigation-panel')).to_have_count(0)
        self.page.get_by_role('button', name='Toggle navigation', exact=True).click()
        expect(self.page.locator('.navigation-panel')).to_be_visible()
        self.page.get_by_role('button', name='Toggle navigation', exact=True).click()
        expect(self.page.locator('.navigation-panel')).to_have_count(0)


if __name__ == '__main__':
    started = time.monotonic()
    suite = unittest.defaultTestLoader.loadTestsFromNames(sys.argv[1:], sys.modules[__name__]) if len(sys.argv) > 1 else unittest.defaultTestLoader.loadTestsFromTestCase(FrontendTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    report = {'mode': 'http' if os.environ.get('HATCHLING_BASE_URL') else 'offline-document', 'browser': 'Chromium via Playwright', 'tests': result.testsRun, 'failures': len(result.failures), 'errors': len(result.errors), 'duration_seconds': round(time.monotonic() - started, 2), 'success': result.wasSuccessful()}
    (ROOT / 'artifacts' / ('browser-selected-results.json' if len(sys.argv) > 1 else 'browser-results.json')).write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(0 if result.wasSuccessful() else 1)
