"""Actual app UI: approvals, original mascot media, and geometric reaction alignment."""
from pathlib import Path
import json
import os
import shutil
import time
import unittest
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'artifacts' / 'approvals-mascot'


class ApprovalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.playwright = sync_playwright().start()
        executable = os.environ.get('CHROMIUM_PATH') or shutil.which('chromium') or shutil.which('chromium-browser')
        cls.browser = cls.playwright.chromium.launch(**({'executable_path': executable} if executable else {}), headless=True, args=['--no-sandbox'])
        cls.html = (ROOT / 'dist' / 'hatchling-standalone.html').read_text()
        OUT.mkdir(parents=True, exist_ok=True)

    @classmethod
    def tearDownClass(cls):
        cls.browser.close()
        cls.playwright.stop()

    def setUp(self):
        self.context = self.browser.new_context(viewport={'width': 1600, 'height': 960}, color_scheme='dark', reduced_motion='reduce')
        self.page = self.context.new_page()
        self.page.set_default_timeout(7000)
        self.errors = []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.set_content(self.html, wait_until='domcontentloaded')
        expect(self.page.locator('#message-input')).to_be_visible()

    def tearDown(self):
        self.context.close()
        self.assertEqual(self.errors, [])

    def scenario(self, name='network'):
        self.page.keyboard.press('Control+Shift+D')
        self.page.locator(f'[data-approval-scenario="{name}"]').click()

    def route(self, route):
        self.page.evaluate('(route) => { location.hash = route; }', route)
        self.page.wait_for_timeout(80)

    def capture(self, name):
        self.page.screenshot(path=str(OUT / f'{name}.png'))

    def assert_no_overflow(self):
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth + 1'))

    def test_01_plus_is_geometrically_centered_and_opens_more_reactions(self):
        row = self.page.locator('.message-row.agent').last
        row.hover()
        row.locator('.reaction-menu summary').click()
        extra = row.get_by_role('button', name='More reactions', exact=True)
        b, s = extra.bounding_box(), extra.locator('svg').bounding_box()
        self.assertLess(abs((b['x'] + b['width']/2) - (s['x'] + s['width']/2)), .51)
        self.assertLess(abs((b['y'] + b['height']/2) - (s['y'] + s['height']/2)), .51)
        self.assertEqual(b['height'], b['width'])
        self.capture('05-reaction-centered')
        extra.click()
        expect(self.page.get_by_role('dialog', name='More reactions', exact=True)).to_be_visible()
        expect(self.page.locator('.reaction-menu[open]')).to_have_count(0)
        self.page.get_by_role('dialog').get_by_role('button', name='🎉', exact=True).click()
        expect(self.page.locator('.reaction-chip')).to_have_text('🎉')

    def test_02_pending_card_sits_above_input_and_matches_history(self):
        self.page.locator('#message-input').fill('Keep my draft while reviewing')
        self.scenario()
        card = self.page.locator('.task-approval-card')
        expect(card).to_contain_text('Task approval needed')
        expect(self.page.locator('.task-approval-record')).to_contain_text('Pending approval')
        self.assertLessEqual(card.bounding_box()['y'] + card.bounding_box()['height'], self.page.locator('.composer').bounding_box()['y'])
        self.assertEqual(card.get_attribute('data-task-approval'), self.page.locator('.task-approval-record').get_attribute('data-approval'))
        expect(self.page.locator('#message-input')).to_have_value('Keep my draft while reviewing')
        expect(self.page.locator('.agent-status')).to_contain_text('Waiting for approval')
        self.capture('02-pending')

    def test_03_allow_once_updates_history_activity_and_keeps_draft(self):
        self.scenario()
        self.page.locator('#message-input').fill('The next thought')
        self.page.locator('[id^="approval-once-"]').click()
        expect(self.page.locator('.task-approval-card')).to_have_count(0)
        expect(self.page.locator('.task-approval-record')).to_contain_text('Allowed once')
        expect(self.page.locator('#message-input')).to_have_value('The next thought')
        self.capture('04-approved')
        self.page.locator('#context-tab-activity').click()
        expect(self.page.locator('.approval-activity-summary')).to_contain_text('Allowed once')
        self.page.get_by_role('button', name='Open evidence: Install project tools', exact=True).click()
        expect(self.page.locator('.activity-dialog')).to_contain_text('Permission record:')
        expect(self.page.locator('.activity-dialog')).to_contain_text('does not install software')

    def test_04_deny_is_not_an_approval_and_removes_only_the_resolved_request(self):
        self.scenario('multiple')
        first = self.page.locator('.task-approval-card').get_attribute('data-task-approval')
        self.page.locator('[id^="approval-deny-"]').click()
        expect(self.page.locator(f'[data-approval="{first}"]')).to_contain_text('Declined')
        expect(self.page.locator('.task-approval-card')).to_be_visible()
        self.assertNotEqual(self.page.locator('.task-approval-card').get_attribute('data-task-approval'), first)
        expect(self.page.locator('.task-approval-card')).to_contain_text('Run the project setup check')
        expect(self.page.locator('[id^="approval-task-"]')).to_have_count(0)

    def test_05_multiple_requests_have_independent_scope_and_navigation(self):
        self.scenario('multiple')
        expect(self.page.locator('.task-approval-pagination')).to_contain_text('1 of 2')
        self.page.locator('#next-task-approval').click()
        expect(self.page.locator('.task-approval-pagination')).to_contain_text('2 of 2')
        expect(self.page.locator('[id^="approval-task-"]')).to_have_count(0)
        self.page.locator('#previous-task-approval').click()
        expect(self.page.locator('[id^="approval-task-"]')).to_be_visible()
        self.capture('06-multiple-requests')
        self.page.locator('[id^="approval-task-"]').click()
        expect(self.page.locator('.approvals-panel')).to_contain_text('Allowed for this task')
        expect(self.page.locator('.task-approval-card')).to_contain_text('Run the project setup check')

    def test_06_failed_decision_is_visible_and_retryable(self):
        self.scenario('failed')
        self.page.locator('[id^="approval-once-"]').click()
        expect(self.page.locator('.task-approval-feedback.error')).to_contain_text('not saved')
        expect(self.page.locator('[id^="approval-once-"]')).to_be_enabled()
        expect(self.page.locator('.task-approval-record')).to_contain_text('Decision not saved')
        self.capture('07-decision-failed')
        self.page.locator('[id^="approval-once-"]').click()
        expect(self.page.locator('.task-approval-card')).to_have_count(0)
        expect(self.page.locator('.task-approval-record')).to_contain_text('Allowed once')

    def test_07_unknown_decision_has_check_status_not_a_second_approve(self):
        self.scenario('unknown')
        self.page.locator('[id^="approval-once-"]').click()
        expect(self.page.locator('.task-approval-feedback.unknown')).to_contain_text('not confirmed')
        expect(self.page.locator('[id^="approval-once-"]')).to_have_count(0)
        expect(self.page.locator('[id^="approval-deny-"]')).to_have_count(0)
        expect(self.page.locator('.task-approval-record')).to_contain_text('Decision not confirmed')
        self.capture('08-decision-unknown')
        self.page.locator('[id^="approval-check-"]').click()
        expect(self.page.locator('.task-approval-record')).to_contain_text('Allowed once')
        expect(self.page.locator('.task-approval-card')).to_have_count(0)

    def test_08_expired_request_has_history_but_no_active_decision_buttons(self):
        self.scenario('expired')
        expect(self.page.locator('.task-approval-card')).to_have_count(0)
        expect(self.page.locator('.task-approval-record')).to_contain_text('Timed out')
        self.page.locator('.task-approval-record summary').click()
        expect(self.page.locator('.approval-report').first).to_contain_text('A new request is needed')
        expect(self.page.get_by_role('button', name='Allow once', exact=True)).to_have_count(0)
        self.capture('09-expired')

    def test_09_offline_does_not_queue_permission_decisions(self):
        self.scenario()
        self.page.keyboard.press('Control+Shift+D')
        self.page.locator('[data-reliability-scenario="offline"]').click()
        expect(self.page.locator('.task-approval-card')).to_contain_text('Permissions are never queued')
        expect(self.page.locator('[id^="approval-once-"]')).to_be_disabled()
        expect(self.page.locator('[id^="approval-deny-"]')).to_be_disabled()
        expect(self.page.locator('.task-approval-record')).to_contain_text('Pending approval')
        self.capture('10-offline-permission')

    def test_10_request_stays_with_original_chat_and_draft(self):
        self.page.locator('#message-input').fill('Draft in the original chat')
        self.scenario()
        self.route('chat/main')
        expect(self.page.locator('.task-approval-card')).to_have_count(0)
        self.page.locator('#message-input').fill('A different draft')
        self.route('chat/reference')
        expect(self.page.locator('.task-approval-card')).to_be_visible()
        expect(self.page.locator('#message-input')).to_have_value('Draft in the original chat')
        self.page.locator('[id^="approval-deny-"]').click()
        self.route('chat/main')
        expect(self.page.locator('#message-input')).to_have_value('A different draft')

    def test_11_activity_points_to_the_same_request(self):
        self.scenario()
        self.page.locator('#context-tab-activity').click()
        expect(self.page.locator('.approval-activity-summary')).to_contain_text('1 approval needed')
        expect(self.page.locator('.activity-entry').first).to_contain_text('approval needed')
        self.capture('03-activity')
        self.page.locator('#activity-review-approvals').click()
        expect(self.page.locator('#context-tab-approvals')).to_have_attribute('aria-selected', 'true')
        expect(self.page.locator('.task-approval-record')).to_contain_text('Pending approval')

    def test_12_scope_stays_open_during_chat_updates_and_escape_never_approves(self):
        self.scenario()
        self.page.locator('.task-approval-details summary').click()
        expect(self.page.locator('.task-approval-details')).to_have_attribute('open', '')
        self.page.locator('#message-input').fill('A note while this request waits.')
        self.page.locator('#message-input').press('Enter')
        expect(self.page.locator('.task-approval-details')).to_have_attribute('open', '')
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.task-approval-card')).to_be_visible()
        self.assertEqual(self.page.locator('.task-approval-record[data-outcome="pending"]').count(), 1)

    def test_13_phone_390_keeps_card_composer_and_history_accessible(self):
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.scenario('multiple')
        expect(self.page.locator('.task-approval-card')).to_be_visible()
        expect(self.page.locator('#message-input')).to_be_visible()
        self.assert_no_overflow()
        self.capture('11-phone-390')
        self.page.locator('#open-task-approval-history').click()
        expect(self.page.locator('.context-panel')).to_be_visible()
        self.capture('12-phone-approvals')
        self.page.locator('.task-approval-record summary').first.click()
        self.page.get_by_role('button', name='Review in chat', exact=True).click()
        expect(self.page.locator('.context-panel')).to_have_count(0)
        expect(self.page.locator('.task-approval-card')).to_be_visible()

    def test_14_phone_320_and_short_height_have_no_hidden_input(self):
        self.page.set_viewport_size({'width': 320, 'height': 740})
        self.scenario('failed')
        self.page.locator('[id^="approval-once-"]').click()
        expect(self.page.locator('.task-approval-feedback.error')).to_be_visible()
        self.assert_no_overflow()
        self.capture('13-phone-320')
        self.page.set_viewport_size({'width': 600, 'height': 400})
        expect(self.page.locator('#message-input')).to_be_visible()
        # Measured where the element is resolved: after a resize the layout can
        # re-render it between the visibility check and a separate measurement.
        bottom = self.page.locator('#message-input').evaluate('(el) => el.getBoundingClientRect().bottom')
        self.assertLessEqual(bottom, 401)
        self.assert_no_overflow()
        self.capture('14-short-window')

    def test_15_light_theme_uses_the_same_layout_and_readable_status(self):
        self.page.emulate_media(color_scheme='light')
        self.scenario()
        expect(self.page.locator('.task-approval-record')).to_contain_text('Pending approval')
        self.assert_no_overflow()
        self.capture('15-light-theme')

    def test_16_full_size_mascot_is_available_in_images(self):
        self.route('library/images')
        expect(self.page.locator('.artifact-meta')).to_contain_text('Hatchling mascot')
        image = self.page.locator('.artifact-preview img').first
        expect(image).to_be_visible()
        self.assertEqual(image.evaluate('(el) => el.naturalWidth'), 1254)
        self.capture('16-library-mascot')
        self.page.locator('.artifact-preview').first.click()
        expect(self.page.locator('.image-lightbox img')).to_be_visible()
        self.assertEqual(self.page.locator('.image-lightbox img').evaluate('(el) => el.naturalWidth'), 1254)
        self.capture('17-mascot-viewer')

    def test_17_video_poster_and_actual_playback_use_the_new_mascot(self):
        self.route('library/videos')
        expect(self.page.locator('.artifact-meta')).to_contain_text('Hatchling · a little hello')
        image = self.page.locator('.video-thumb img')
        self.assertEqual(image.evaluate('(el) => [el.naturalWidth,el.naturalHeight]'), [360, 640])
        self.capture('18-library-video')
        self.page.locator('.artifact-preview').first.click()
        video = self.page.locator('video.media-player')
        expect(video).to_be_visible()
        self.page.wait_for_function('() => document.querySelector("video.media-player").readyState >= 2')
        self.assertEqual(video.evaluate('(el) => [el.videoWidth,el.videoHeight,Math.round(el.duration)]'), [360,640,4])
        video.evaluate('(el) => { el.muted = true; return el.play(); }')
        self.page.wait_for_timeout(700)
        self.assertGreater(video.evaluate('(el) => el.currentTime'), .1)
        video.evaluate('(el) => el.pause()')
        self.capture('19-mascot-video-playing')

    def test_18_purchase_prompt_requires_full_exact_quote_review(self):
        self.page.keyboard.press('Control+Shift+D')
        self.page.locator('[data-scenario="purchase"]').click()
        expect(self.page.locator('.domain-approval-card')).to_contain_text('Review purchase')
        expect(self.page.locator('.domain-approval-card').get_by_role('button', name='Allow once')).to_have_count(0)
        self.page.locator('#review-composer-purchase').click()
        expect(self.page.locator('.purchase-card')).to_have_attribute('data-payment-state', 'proposed')
        self.assertEqual(self.page.locator('.purchase-card').evaluate('(el) => document.activeElement === el'), True)
        self.page.locator('#context-tab-approvals').click()
        expect(self.page.locator('[data-domain-history]').first).to_contain_text('Pending approval')
        self.capture('20-purchase-approval')

    def test_19_keyboard_only_permission_controls_and_focus_are_usable(self):
        self.scenario()
        self.page.locator('.task-approval-details summary').focus()
        self.page.keyboard.press('Enter')
        expect(self.page.locator('.task-approval-details')).to_have_attribute('open', '')
        self.page.locator('[id^="approval-deny-"]').focus()
        self.page.keyboard.press('Enter')
        expect(self.page.locator('.task-approval-record')).to_contain_text('Declined')
        expect(self.page.locator('#message-input')).to_be_focused()


    def test_20_unresolved_permission_prevents_deleting_its_chat(self):
        self.scenario()
        self.page.get_by_label('Manage chat Organize a sample project', exact=True).click()
        expect(self.page.get_by_role('button', name='Delete chat', exact=True)).to_be_disabled()
        self.page.keyboard.press('Escape')
        self.page.locator('[id^="approval-deny-"]').click()
        self.page.get_by_label('Manage chat Organize a sample project', exact=True).click()
        expect(self.page.get_by_role('button', name='Delete chat', exact=True)).to_be_enabled()

    def test_21_browser_permission_remains_above_input_in_split_workspace(self):
        self.page.keyboard.press('Control+Shift+D')
        self.page.locator('[data-scenario="browser-watch"]').click()
        expect(self.page.locator('#allow-composer-challenge')).to_be_visible(timeout=12000)
        self.page.locator('#view-composer-challenge').click()
        expect(self.page.locator('.browser-session-dock')).to_be_visible()
        expect(self.page.locator('.domain-approval-card')).to_be_visible()
        self.assert_no_overflow()
        self.capture('21-browser-approval')
        self.page.locator('#allow-composer-challenge').click()
        expect(self.page.locator('.domain-approval-card')).to_have_count(0)

    def test_22_large_text_preserves_card_actions_on_mobile(self):
        self.page.locator('#rail-more').click()
        self.page.locator('.rail-more').get_by_role('button', name='Settings', exact=True).click()
        self.page.get_by_role('radio', name='Large', exact=True).click()
        self.page.keyboard.press('Escape')
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.scenario()
        expect(self.page.locator('[id^="approval-once-"]')).to_be_visible()
        expect(self.page.locator('#message-input')).to_be_visible()
        self.assert_no_overflow()
        self.capture('22-large-text-mobile')

if __name__ == '__main__':
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(ApprovalTests))
    (OUT / 'approvals-browser-results.json').write_text(json.dumps({'tests': result.testsRun, 'failures': len(result.failures), 'errors': len(result.errors), 'success': result.wasSuccessful(), 'duration_seconds': round(time.monotonic() - start, 2)}, indent=2) + '\n')
    raise SystemExit(0 if result.wasSuccessful() else 1)
