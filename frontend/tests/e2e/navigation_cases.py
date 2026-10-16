"""Revision 9: route identity, not browser-history heuristics. Actual DOM interactions."""
import re
from playwright.sync_api import expect

class NavigationCases:
    def collapse_chat_navigation(self):
        if self.page.locator('.navigation-panel').count():
            self.page.locator('#chat-navigation-toggle').click()
        expect(self.page.locator('#chat-navigation-toggle')).to_have_attribute('aria-expanded', 'false')

    def test_143_main_uses_chats_pill_and_never_a_main_back_arrow(self):
        self.route('chat')
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        expect(self.page.get_by_role('button', name='Back to Main chat', exact=True)).to_have_count(0)
        self.collapse_chat_navigation()
        expect(self.page.locator('.conversation-toolbar')).to_be_visible()
        self.page.locator('#chat-navigation-toggle').click()
        expect(self.page.locator('#chat-navigation-toggle')).to_have_attribute('aria-expanded', 'true')
        expect(self.page.locator('#chat-search')).to_be_focused()
        expect(self.page.locator('#chat-navigation-toggle')).to_have_attribute('aria-controls', 'chat-navigation')

    def test_144_side_header_uses_title_and_back_goes_to_main_not_previous_page(self):
        self.route('goals')
        self.route('chat/reference')
        self.collapse_chat_navigation()
        expect(self.page.locator('.conversation-title')).to_have_text('Organize a sample project')
        self.page.get_by_role('button', name='Back to Main chat', exact=True).click()
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        expect(self.page.locator('.navigation-panel')).to_have_count(0)
        expect(self.page.locator('#chat-navigation-toggle')).to_be_focused()
        self.assertEqual(self.page.evaluate('location.hash'), '#chat/main')

    def test_145_header_back_retains_separate_drafts_and_scroll_positions(self):
        self.page.locator('#message-input').fill('Side-chat draft')
        stream = self.page.locator('.chat-scroll')
        stream.evaluate('(el)=>el.scrollTop=110')
        self.page.wait_for_timeout(50)
        self.collapse_chat_navigation()
        saved = stream.evaluate('(el)=>el.scrollTop')
        self.page.locator('#chat-back').click()
        self.page.locator('#message-input').fill('Main draft')
        self.page.evaluate('history.back()')
        expect(self.page.locator('.conversation-title')).to_have_text('Organize a sample project')
        expect(self.page.locator('#message-input')).to_have_value('Side-chat draft')
        self.assertAlmostEqual(stream.evaluate('(el)=>el.scrollTop'), saved, delta=2)
        self.page.evaluate('history.forward()')
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        expect(self.page.locator('#message-input')).to_have_value('Main draft')

    def test_146_stale_or_deleted_chat_links_do_not_show_another_transcript_under_a_wrong_id(self):
        self.route('chat/no-longer-exists')
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        self.assertEqual(self.page.evaluate('location.hash'), '#chat/main')
        expect(self.page.locator('.toast').filter(has_text='no longer available')).to_be_visible()
        self.page.locator('#message-input').fill('A main-only message')
        self.page.get_by_role('button', name='Send message', exact=True).click()
        expect(self.page.locator('.user-bubble').filter(has_text='A main-only message')).to_be_visible()
        self.route('chat/reference')
        expect(self.page.locator('.user-bubble').filter(has_text='A main-only message')).to_have_count(0)

    def test_147_escape_closes_list_restores_focus_without_navigation_or_draft_loss(self):
        self.page.locator('#message-input').fill('Do not lose this.')
        self.collapse_chat_navigation()
        self.page.locator('#chat-navigation-toggle').click()
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.navigation-panel')).to_have_count(0)
        expect(self.page.locator('#chat-navigation-toggle')).to_be_focused()
        expect(self.page.locator('#message-input')).to_have_value('Do not lose this.')
        expect(self.page.locator('.conversation-title')).to_have_text('Organize a sample project')

    def test_148_long_titles_truncate_without_covering_avatar_and_have_full_tooltip(self):
        title = 'Sample planning notebook — outlines, examples and documentation'
        self.page.get_by_label('Manage chat Organize a sample project', exact=True).click()
        self.page.get_by_role('button', name='Rename chat', exact=True).click()
        self.page.get_by_label('Chat name', exact=True).fill(title)
        self.page.get_by_role('button', name='Save name', exact=True).click()
        self.collapse_chat_navigation()
        expect(self.page.locator('.conversation-title')).to_have_text(title)
        self.page.locator('#chat-navigation-toggle').hover()
        expect(self.page.locator('#workspace-tooltip')).to_have_text(title)
        self.assertGreater(self.page.locator('.conversation-title').evaluate('(el)=>el.scrollWidth-el.clientWidth'), 5)
        self.page.locator('.context-close button').click()
        for width in [1440, 900, 390, 320]:
            self.page.set_viewport_size({'width':width, 'height':844})
            # Viewport changes replace the shell. Measure both nodes atomically after reflow,
            # rather than retaining an element handle while the resize render detaches it.
            self.page.evaluate('() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))')
            geometry = self.page.wait_for_function('''() => {
                const header=document.querySelector('.conversation-location');
                const avatar=document.querySelector('.floating-agent-toggle');
                if (!header || !avatar) return false;
                const a=header.getBoundingClientRect(), b=avatar.getBoundingClientRect();
                return a.width && b.width ? {a:a.toJSON(), b:b.toJSON()} : false;
            }''').json_value()
            a=geometry['a']; b=geometry['b']
            overlap=max(0,min(a['x']+a['width'],b['x']+b['width'])-max(a['x'],b['x']))*max(0,min(a['y']+a['height'],b['y']+b['height'])-max(a['y'],b['y']))
            self.assertEqual(overlap, 0, f'Avatar and breadcrumb overlap at {width}')
            self.assert_no_overflow()

    def test_149_mobile_navigation_backdrop_dismisses_then_header_back_remains_usable(self):
        for width in [320,390]:
            self.page.set_viewport_size({'width':width,'height':760})
            self.route('chat/reference')
            self.page.locator('#chat-navigation-toggle').click()
            expect(self.page.locator('.navigation-panel')).to_be_visible()
            self.page.keyboard.press('Escape')
            expect(self.page.locator('.navigation-panel')).to_have_count(0)
            self.page.locator('#chat-back').click()
            expect(self.page.locator('.conversation-title')).to_have_text('Chats')
            self.assert_no_overflow()

    def test_150_nested_menu_escape_does_not_close_the_chat_list(self):
        self.page.locator('.side-chat-options summary').click()
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.navigation-panel')).to_be_visible()
        expect(self.page.locator('.side-chat-options')).not_to_have_attribute('open','')
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.navigation-panel')).to_have_count(0)

    def test_151_header_updates_after_rename_and_browser_history_preserves_identity(self):
        self.page.get_by_label('Manage chat Organize a sample project', exact=True).click()
        self.page.get_by_role('button', name='Rename chat', exact=True).click()
        self.page.get_by_label('Chat name', exact=True).fill('Main chat')
        self.page.get_by_role('button', name='Save name', exact=True).click()
        expect(self.page.locator('.conversation-title')).to_have_text('Main chat')
        expect(self.page.get_by_role('button', name='Back to Main chat', exact=True)).to_be_visible()
        self.page.locator('#chat-back').click()
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')

    def test_152_chat_pill_does_not_override_channel_read_only_behavior(self):
        self.scenario('channels')
        self.page.keyboard.press('Escape')
        row=self.page.locator('#chat-link-chat-whatsapp')
        row.click()
        expect(self.page.locator('.conversation-title')).to_contain_text('WhatsApp')
        expect(self.page.locator('#message-input')).to_have_count(0)
        self.page.locator('#chat-back').click()
        expect(self.page.locator('#message-input')).to_be_visible()
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')

    def test_153_navigation_motion_runs_once_and_reduced_motion_is_immediate(self):
        self.page.emulate_media(reduced_motion='no-preference')
        self.collapse_chat_navigation()
        self.page.locator('#chat-navigation-toggle').click()
        self.assertGreater(self.page.locator('.navigation-panel').evaluate('(el)=>el.getAnimations().length'),0)
        self.page.wait_for_timeout(230)
        self.assertEqual(self.page.locator('.navigation-panel').evaluate('(el)=>el.getAnimations().length'),0)
        self.page.emulate_media(reduced_motion='reduce')
        self.collapse_chat_navigation()
        self.page.locator('#chat-navigation-toggle').click()
        self.assertEqual(self.page.locator('.navigation-panel').evaluate('(el)=>el.getAnimations().length'),0)

    def test_154_stream_updates_do_not_reopen_a_closed_list_or_rename_the_header(self):
        self.page.clock.install()
        self.scenario('bundle-files')
        self.route('chat/reference')
        self.collapse_chat_navigation()
        label=self.page.locator('.conversation-title').inner_text()
        self.page.locator('#message-input').fill('Still writing while work arrives')
        self.page.clock.fast_forward(14000)
        expect(self.page.locator('.conversation-title')).to_have_text(label)
        expect(self.page.locator('.navigation-panel')).to_have_count(0)
        expect(self.page.locator('#message-input')).to_have_value('Still writing while work arrives')
        expect(self.page.locator('.native-file-card').filter(has_text='Sample handoff tarball')).to_be_visible()

    def delayed_transport(self, succeeds):
        """Test-only adapter wrapper, injected into a copied document. Never shipped in the app.
        Its promise models a slow transport without adding a protocol member or a production hook.
        """
        import hashlib, base64
        from pathlib import Path
        html=(Path(__file__).resolve().parents[2]/'dist/hatchling-standalone.html').read_text()
        match=re.search(r'<script>([\s\S]*?)</script>',html)
        assert match is not None
        script=match.group(1)
        old='const app = new controller_js_1.App(adapter, root, dialogs, toasts);'
        assert script.count(old)==1, 'Test adapter mount no longer matches the compiled entry; review it.'
        # Opened by set_content, the page has no token: adapter is the scripted one.
        replacement='''const delayed = adapter;
const executeOriginal = delayed.execute.bind(delayed);
let waiting = true;
delayed.execute = command => {
    if (command.type === 'chat.send' && waiting) {
        waiting = false;
        return new Promise(resolve => { window.__settleTestSend = async () => { resolve(OUTCOME); }; });
    }
    return executeOriginal(command);
};
const app = new controller_js_1.App(delayed, root, dialogs, toasts);'''.replace('OUTCOME', 'await executeOriginal(command)' if succeeds else "{ok:false,message:'The test transport rejected this message.'}")
        modified=script.replace(old,replacement)
        digest=base64.b64encode(hashlib.sha256(modified.encode()).digest()).decode()
        html=html.replace(script,modified).replace(re.search(r"sha256-[A-Za-z0-9+/=]+",html).group(0),'sha256-'+digest)
        # A new document releases the previous app and its subscriptions.
        self.page.close()
        self.page=self.context.new_page()
        self.page.set_default_timeout(5000)
        self.page.on('pageerror',lambda error:self.errors.append(str(error)))
        self.page.set_content(html,wait_until='domcontentloaded')
        expect(self.page.locator('#message-input')).to_be_visible()

    def test_155_late_failed_send_recovers_only_in_its_original_chat(self):
        self.delayed_transport(False)
        self.page.locator('#message-input').fill('A delayed side-chat message')
        self.page.get_by_role('button',name='Send message',exact=True).click()
        expect(self.page.locator('#message-input')).to_have_value('')
        self.page.locator('#chat-back').click()
        self.page.locator('#message-input').fill('My new Main chat draft')
        self.page.wait_for_function("() => typeof window.__settleTestSend === 'function'")
        self.page.evaluate('window.__settleTestSend()')
        expect(self.page.locator('#message-input')).to_have_value('My new Main chat draft')
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        self.route('chat/reference')
        expect(self.page.locator('.delivery-failed')).to_contain_text('A delayed side-chat message')
        expect(self.page.locator('#message-input')).to_have_value('')
        self.page.locator('.delivery-failed').get_by_role('button', name='Edit', exact=True).click()
        expect(self.page.locator('#message-input')).to_have_value('A delayed side-chat message')

    def test_156_late_failed_send_preserves_text_typed_while_waiting(self):
        self.delayed_transport(False)
        count=self.page.locator('.user-bubble').count()
        self.page.locator('#message-input').fill('First message')
        self.page.get_by_role('button',name='Send message',exact=True).click()
        self.page.locator('#message-input').fill('My next thought')
        self.page.wait_for_function("() => typeof window.__settleTestSend === 'function'")
        self.page.evaluate('window.__settleTestSend()')
        expect(self.page.locator('#message-input')).to_have_value('My next thought')
        expect(self.page.locator('.delivery-failed')).to_contain_text('First message')
        self.page.locator('.delivery-failed').get_by_role('button', name='Edit', exact=True).click()
        expect(self.page.locator('#message-input')).to_have_value('First message\n\nMy next thought')
        self.assertEqual(self.page.locator('.user-bubble').count(),count)

    def test_157_late_success_does_not_steal_focus_or_scroll_from_another_chat(self):
        self.delayed_transport(True)
        self.page.locator('#message-input').fill('A successful delayed message')
        self.page.get_by_role('button',name='Send message',exact=True).click()
        self.page.locator('#chat-back').click()
        self.page.locator('.chat-scroll').evaluate('(el)=>el.scrollTop=5')
        self.page.locator('#chat-navigation-toggle').focus()
        top=self.page.locator('.chat-scroll').evaluate('(el)=>el.scrollTop')
        self.page.wait_for_function("() => typeof window.__settleTestSend === 'function'")
        self.page.evaluate('window.__settleTestSend()')
        expect(self.page.locator('.conversation-title')).to_have_text('Chats')
        expect(self.page.locator('#chat-navigation-toggle')).to_be_focused()
        self.assertAlmostEqual(self.page.locator('.chat-scroll').evaluate('(el)=>el.scrollTop'),top,delta=2)
        self.route('chat/reference')
        expect(self.page.locator('.user-bubble').filter(has_text='A successful delayed message')).to_be_visible()

    def test_158_escape_dismisses_settings_without_also_closing_the_list_or_document(self):
        self.page.keyboard.press('Control+,')
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.settings-dialog')).to_have_count(0)
        expect(self.page.locator('.navigation-panel')).to_be_visible()
        self.page.get_by_role('button',name='Edit',exact=True).click()
        expect(self.page.locator('.identity-document-dock')).to_be_visible()
        self.page.keyboard.press('Control+,')
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.settings-dialog')).to_have_count(0)
        expect(self.page.locator('.identity-document-dock')).to_be_visible()
