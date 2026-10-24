"""V10: typographic lockup, stable status DOM, entrance motion, synthetic release assets."""
from playwright.sync_api import expect

class ReleaseCases:
    def restore_profile(self, animated=True):
        self.page.emulate_media(reduced_motion='no-preference' if animated else 'reduce')
        self.page.locator('.context-close button').click()
        self.page.wait_for_timeout(400 if animated else 20)
        self.page.get_by_role('button', name='Show agent status', exact=True).click()

    def test_159_profile_typography_matches_reference_lockup_at_native_zoom(self):
        profile = self.page.locator('.agent-profile')
        values = profile.evaluate('''el => {
          const name=el.querySelector('.agent-name'), status=el.querySelector('.agent-status');
          return {name:getComputedStyle(name).fontSize, weight:getComputedStyle(name).fontWeight,
            status:getComputedStyle(status).fontSize, color:getComputedStyle(status).color,
            portrait:el.querySelector('.agent-portrait > img').getBoundingClientRect().width,
            glyph:status.querySelector('svg').getBoundingClientRect().width, scale:visualViewport.scale};
        }''')
        self.assertEqual(values['name'],'23px')
        self.assertEqual(values['weight'],'500')
        self.assertEqual(values['status'],'18px')
        self.assertEqual(values['color'],'rgb(139, 142, 147)')
        self.assertAlmostEqual(values['portrait'],100,delta=.1)
        self.assertEqual(values['glyph'],16)
        self.assertEqual(values['scale'],1)
        expect(profile.locator('.agent-status')).to_have_attribute('title','Connected to the local scripted adapter. No live agent.')

    def test_160_profile_return_staggers_name_and_status_in_actual_animations(self):
        self.restore_profile()
        state = self.page.locator('.agent-profile').evaluate('''el => ['.agent-name','.agent-status'].map(selector=>{
          const node=el.querySelector(selector), a=node.getAnimations().find(item=>item.id.startsWith('profile-'));
          return {id:a?.id,delay:a?.effect.getTiming().delay,duration:a?.effect.getTiming().duration,
              opacity:getComputedStyle(node).opacity,transform:getComputedStyle(node).transform};
        })''')
        self.assertEqual(state[0]['id'],'profile-name-return')
        self.assertEqual(state[1]['id'],'profile-status-return')
        self.assertLess(state[0]['delay'],state[1]['delay'])
        self.assertLess(float(state[1]['opacity']),1)
        self.page.wait_for_timeout(500)
        for selector in ['.agent-name','.agent-status']:
            self.assertEqual(self.page.locator(selector).evaluate('el=>getComputedStyle(el).opacity'),'1')
            self.assertEqual(self.page.locator(selector).evaluate('el=>getComputedStyle(el).transform'),'none')
        expect(self.page.locator('.agent-transition-ghost')).to_have_count(0)

    def test_161_rapid_panel_toggles_never_leave_a_hidden_avatar_or_text(self):
        self.page.emulate_media(reduced_motion='no-preference')
        for _ in range(3):
            self.page.locator('.context-close button').click()
            self.page.get_by_role('button',name='Show agent status',exact=True).click()
        self.page.wait_for_timeout(550)
        expect(self.page.locator('.agent-transition-ghost')).to_have_count(0)
        self.assertEqual(self.page.locator('.agent-portrait > img').evaluate('el=>getComputedStyle(el).visibility'),'visible')
        self.assertEqual(self.page.locator('.agent-name').evaluate('el=>getComputedStyle(el).opacity'),'1')

    def test_162_reduced_motion_restores_text_immediately_without_flights(self):
        self.restore_profile(False)
        self.assertEqual(self.page.locator('.agent-name').evaluate('el=>el.getAnimations().length'),0)
        self.assertEqual(self.page.locator('.agent-status').evaluate('el=>el.getAnimations().length'),0)
        expect(self.page.locator('.agent-transition-ghost')).to_have_count(0)
        expect(self.page.locator('.agent-name')).to_be_visible()

    def test_163_enabling_reduced_motion_during_a_return_cancels_text_motion(self):
        self.restore_profile()
        self.page.emulate_media(reduced_motion='reduce')
        self.page.wait_for_timeout(60)
        expect(self.page.locator('.agent-transition-ghost')).to_have_count(0)
        for selector in ['.agent-name','.agent-status']:
            self.assertEqual(self.page.locator(selector).evaluate('el=>getComputedStyle(el).opacity'),'1')
            self.assertEqual(self.page.locator(selector).evaluate('el=>el.getAnimations().length'),0)

    def test_164_unchanged_snapshot_keeps_profile_dom_and_does_not_replay_motion(self):
        self.page.emulate_media(reduced_motion='no-preference')
        self.page.locator('.agent-profile').evaluate('el=>window.__profileProof=el')
        self.page.locator('#message-input').fill('Keep this sample note.')
        self.page.get_by_role('button',name='Send message',exact=True).click()
        self.assertTrue(self.page.locator('.agent-profile').evaluate('el=>el===window.__profileProof'))
        self.assertEqual(self.page.locator('.agent-name').evaluate('el=>el.getAnimations().length'),0)

    def test_165_name_change_animates_only_the_changed_label(self):
        self.page.emulate_media(reduced_motion='no-preference')
        self.page.locator('#message-input').fill('Change my name to Pip')
        self.page.get_by_role('button',name='Send message',exact=True).click()
        expect(self.page.locator('.agent-name')).to_have_text('Pip')
        self.page.wait_for_timeout(20)
        self.assertIn('profile-name-update',self.page.locator('.agent-name').evaluate('el=>el.getAnimations().map(a=>a.id)'))
        self.assertEqual(self.page.locator('.agent-status-label').evaluate('el=>el.getAnimations().length'),0)
        self.page.wait_for_timeout(320)
        expect(self.page.locator('.agent-portrait > img')).to_have_attribute('alt','Pip avatar')

    def test_166_long_profile_name_reflows_above_tabs_on_phone(self):
        name='A thoughtful little helper for everyday sample projects'
        self.page.locator('#message-input').fill('Change my name to '+name)
        self.page.get_by_role('button',name='Send message',exact=True).click()
        expect(self.page.locator('.agent-name')).to_have_text(name)
        toggle=self.page.get_by_role('button',name='Show agent status',exact=True)
        for width in [390,320]:
            self.page.set_viewport_size({'width':width,'height':760})
            if width==390:
                # Narrowing from 1919 past 1100 closes the status panel, but
                # only once the page handles the resize, which can come after
                # this call returns: wait for the toggle that replaces it,
                # rather than counting it before it's there.
                expect(toggle).to_be_visible()
                toggle.click()
            # Restoring the status animates it in: measure once it's shown.
            expect(self.page.locator('.agent-status')).to_be_visible()
            expect(self.page.locator('.context-tabs')).to_be_visible()
            # Viewport changes replace the shell, so a node seen a moment ago
            # may have no box now: measure both at once, after the reflow.
            geometry=self.page.wait_for_function('''() => {
                const status=document.querySelector('.agent-status');
                const tabs=document.querySelector('.context-tabs');
                if (!status || !tabs) return false;
                const a=status.getBoundingClientRect(), b=tabs.getBoundingClientRect();
                return a.height && b.height ? {a:a.toJSON(), b:b.toJSON()} : false;
            }''').json_value()
            a=geometry['a'];b=geometry['b']
            self.assertLess(a['y']+a['height'],b['y'])
            self.assert_no_overflow()

    def test_167_actual_work_changes_status_without_restarting_the_name(self):
        self.page.emulate_media(reduced_motion='no-preference')
        self.scenario('fix-ci')
        expect(self.page.locator('.agent-status-label')).to_have_text('Fixing CI')
        self.page.wait_for_timeout(700)
        self.page.locator('.agent-name').evaluate('el=>window.__nameProof=el')
        expect(self.page.locator('.agent-status-label')).to_have_text('Checking tests',timeout=4000)
        self.assertTrue(self.page.locator('.agent-name').evaluate('el=>el===window.__nameProof'))
        self.assertEqual(self.page.locator('.agent-name').evaluate('el=>el.getAnimations().length'),0)
        expect(self.page.locator('.agent-status-label')).to_have_text('Connected',timeout=5000)

    def test_168_reference_illustrations_are_replaced_with_original_local_media(self):
        self.route('feed')
        post=self.page.locator('[data-post="ref-feed-companion"]')
        expect(post).to_contain_text('fictional edition')
        expect(post.locator('img')).to_have_attribute('alt','Original diagram of a sample notebook and pencil')
        post.get_by_role('button',name='Enlarge image: A little more room for your next idea',exact=True).click()
        expect(self.page.get_by_role('dialog')).to_contain_text('Original local illustration')

    def test_169_profile_close_has_no_tooltip_after_hover_or_focus(self):
        close=self.page.locator('.context-close button')
        close.hover();self.page.wait_for_timeout(400)
        self.assertFalse(close.get_attribute('title'))
        self.assertFalse(close.get_attribute('data-tooltip'))
        expect(self.page.locator('#workspace-tooltip')).not_to_be_visible()
        close.focus();expect(close).to_be_focused()

    def test_170_large_profile_remains_readable_and_uses_independent_text_scale(self):
        self.open_settings();self.page.get_by_role('radio',name='Large',exact=True).click();self.page.keyboard.press('Escape')
        self.assertEqual(self.page.locator('.agent-name').evaluate('el=>getComputedStyle(el).fontSize'),'24px')
        self.assertEqual(self.page.locator('.agent-status').evaluate('el=>getComputedStyle(el).fontSize'),'20px')
        self.assert_no_overflow()

    def test_171_opening_scenarios_never_reports_a_missing_conversation(self):
        # Starting from a real side chat must not route through the empty-string id.
        for name in ['browser-watch','choices','fix-ci']:
            self.route('chat/reference')
            self.page.keyboard.press('Control+Shift+D')
            self.page.locator(f'[data-scenario="{name}"]').click()
            expect(self.page.locator('.conversation-toolbar')).to_have_attribute('data-chat-id','main')
            expect(self.page.locator('.toast')).to_have_count(0)
        self.assert_no_overflow()
