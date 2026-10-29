"""V8: native scaling, document-based identity and explicitly opt-in agent control."""
from playwright.sync_api import expect
import re

class PolishCases:
    def identity_document(self):
        self.page.get_by_role('tab', name='Identity', exact=True).click()
        self.page.get_by_role('button', name='Edit', exact=True).click()
        expect(self.page.locator('.identity-document-dock')).to_be_visible()
        return self.page.locator('.identity-document-dock')

    def agent_enabled(self):
        self.open_settings()
        self.page.get_by_role('button', name='Help & support', exact=True).click()
        self.page.get_by_role('button', name='Agent control', exact=True).click()
        self.page.get_by_role('button', name='Enable for this tab', exact=True).click()
        self.page.get_by_role('button', name='Close Agent control', exact=True).click()
        self.page.get_by_role('button', name='Close Settings', exact=True).click()
        self.assertTrue(self.agent_call('status')['enabled'])

    def agent_call(self, method, args=None):
        return self.page.evaluate('''({method,args})=> {
            const api=window.hatchlingFrontend;
            if(method==='read') return api.read(args.resource,args.id);
            if(method==='execute') return api.execute(args);
            if(method==='result') return api.result(args);
            return api[method]();
        }''',{'method':method,'args':args})

    def action(self,key,action):
        rev=self.agent_call('inspect')['revision']
        return self.agent_call('execute',{'key':key,'action':action,'expectedRevision':rev})

    def test_127_comfortable_default_is_larger_without_zoom_or_transform(self):
        expect(self.page.locator('html')).to_have_attribute('data-interface-size','comfortable')
        font=lambda: self.page.locator('#message-input').evaluate('(el)=>parseFloat(getComputedStyle(el).fontSize)')
        self.assertAlmostEqual(font(),17.92,delta=.1)
        initial_height=self.page.locator('.composer').bounding_box()['height']
        self.open_settings()
        self.page.get_by_role('radio',name='Compact',exact=True).click()
        self.page.keyboard.press('Escape')
        self.assertEqual(font(),16)
        self.assertLess(self.page.locator('.composer').bounding_box()['height'],initial_height)
        self.assertEqual(self.page.evaluate('visualViewport.scale'),1)
        self.assertEqual(self.page.locator('html').evaluate('(el)=>getComputedStyle(el).zoom'),'1')
        self.open_settings();self.page.get_by_role('radio',name='Large',exact=True).click();self.page.keyboard.press('Escape')
        self.assertEqual(font(),20)
        self.assert_no_overflow()

    def test_128_more_icon_has_two_equal_centered_strokes_and_four_real_entries(self):
        icon=self.page.locator('#rail-more > svg')
        self.assertEqual(icon.locator('path').evaluate_all('(els)=>els.map(el=>el.getAttribute("d"))'),['M5 9h14','M5 15h14'])
        self.page.locator('#rail-more').click()
        menu=self.page.locator('.rail-more .menu-popover')
        for name in ['Keyboard shortcuts','Download apps','Report an issue','Settings']:
            expect(menu.get_by_role('button',name=re.compile('^'+name))).to_be_visible()
        self.viewport_contains(menu,8)

    def test_129_identity_opens_full_document_retains_rail_and_excludes_notice_from_editor(self):
        dock=self.identity_document()
        expect(self.page.locator('dialog:modal')).to_have_count(0)
        expect(dock).to_have_class(re.compile('dock-expanded'))
        expect(self.page.locator('.rail-avatar')).to_be_visible()
        self.assertAlmostEqual(dock.bounding_box()['x'],self.page.locator('.icon-rail').bounding_box()['width'],delta=1)
        self.assertFalse(self.page.locator('#app').evaluate('(el)=>el.inert'))
        self.assertTrue(self.page.locator('.main-region').evaluate('(el)=>el.inert'))
        expect(dock.locator('.document-about')).to_contain_text('This note is not part of the file')
        dock.locator('.menu > summary').click()
        dock.get_by_role('button',name='Edit document',exact=True).click()
        editor=dock.get_by_role('textbox',name='IDENTITY.md',exact=True)
        self.assertNotIn('About this file',editor.input_value())
        self.assertIn('**Character:**',editor.input_value())

    def test_130_identity_unsaved_draft_survives_navigation_and_can_cancel(self):
        dock=self.identity_document();dock.locator('.menu > summary').click();dock.get_by_role('button',name='Edit document',exact=True).click()
        editor=dock.get_by_role('textbox',name='IDENTITY.md',exact=True)
        original=editor.input_value();editor.fill(original+'\n\nCustom note to retain.')
        self.page.get_by_role('button',name='Goals',exact=True).click()
        expect(self.page.locator('.artifact-dock')).to_have_count(0)
        expect(self.page.get_by_role('heading',name='Goals',level=1)).to_be_visible()
        self.route('chat');dock=self.identity_document()
        expect(dock.get_by_role('textbox',name='IDENTITY.md',exact=True)).to_have_value(original+'\n\nCustom note to retain.')
        dock.get_by_role('button',name='Cancel edits',exact=True).click()
        expect(dock.locator('.document-workspace')).not_to_contain_text('Custom note to retain.')

    def test_131_identity_rejects_invalid_field_without_losing_draft(self):
        dock=self.identity_document();dock.locator('.menu > summary').click();dock.get_by_role('button',name='Edit document',exact=True).click()
        editor=dock.get_by_role('textbox',name='IDENTITY.md',exact=True)
        bad=editor.input_value().replace('**Name:** hatchling','**Name:** ');editor.fill(bad)
        dock.get_by_role('button',name='Save document',exact=True).click()
        expect(editor).to_have_value(bad);expect(editor).to_be_enabled()
        expect(self.page.locator('.toast').last).to_contain_text('Name')

    def test_132_large_phone_settings_identity_and_headers_reflow(self):
        for width in [320,390]:
            self.page.set_viewport_size({'width':width,'height':760})
            self.open_settings();self.page.get_by_role('radio',name='Large',exact=True).click()
            self.assert_no_overflow()
            self.page.locator('.settings-pane').evaluate('(el)=>el.scrollTop=el.scrollHeight')
            self.viewport_contains(self.page.get_by_role('radio',name='Large',exact=True),8)
            self.page.keyboard.press('Escape')
            self.page.get_by_role('button',name='Show agent status',exact=True).click()
            dock=self.identity_document()
            self.assert_no_overflow()
            self.viewport_contains(dock.get_by_role('button',name='Close IDENTITY.md',exact=True),0)
            self.page.keyboard.press('Escape')
            if self.page.locator('.context-close button').count():
                self.page.locator('.context-close button').click()

    def test_133_agent_api_disabled_by_default_no_mutation_or_read_without_opt_in(self):
        self.assertFalse(self.agent_call('status')['enabled'])
        self.assertIn('error',self.agent_call('inspect'))
        self.assertIn('error',self.agent_call('read',{'resource':'document','id':'identity-profile'}))
        result=self.agent_call('execute',{'key':'a','action':{'type':'ui.navigate','route':'goals'}})
        self.assertEqual(result['status'],'blocked')
        self.assertEqual(len(self.agent_call('list')),114)

    def test_134_enabled_agent_uses_shared_navigation_tabs_documents_and_no_auto_send(self):
        self.agent_enabled()
        count=self.page.locator('.message-row').count()
        self.assertEqual(self.action('draft',{'type':'ui.prefill','text':'Consider this.'})['status'],'done')
        expect(self.page.locator('#message-input')).to_have_value('Consider this.')
        self.assertEqual(self.page.locator('.message-row').count(),count)
        self.assertEqual(self.action('tab',{'type':'ui.context','tab':'schedule','open':True})['status'],'done')
        expect(self.page.get_by_role('tab',name='Schedule',exact=True)).to_have_attribute('aria-selected','true')
        self.action('doc',{'type':'ui.document','id':'identity-profile'})
        expect(self.page.locator('.identity-document-dock')).to_be_visible()
        self.action('closedoc',{'type':'ui.close-workspace'})
        self.action('goals',{'type':'ui.navigate','route':'goals'})
        expect(self.page.get_by_role('heading',name='Goals',level=1)).to_be_visible()

    def test_135_agent_mutation_requires_human_review_and_one_time_receipt(self):
        self.agent_enabled();revision=self.agent_call('inspect')['revision']
        request={'key':'rename','expectedRevision':revision,'action':{'type':'settings.name','value':'Pip'}}
        result=self.agent_call('execute',request);self.assertEqual(result['status'],'pending')
        expect(self.page.locator('.agent-profile h2')).to_contain_text('hatchling')
        self.assertEqual(self.agent_call('execute',request)['id'],result['id'])
        # Synthetic DOM clicks are not a user grant.
        self.page.get_by_role('button',name='Approve this change',exact=True).evaluate('(el)=>el.click()')
        self.assertEqual(self.agent_call('result',result['id'])['status'],'pending')
        self.page.get_by_role('button',name='Approve this change',exact=True).click()
        expect(self.page.get_by_role('dialog',name='Review agent action',exact=True)).to_have_count(0)
        self.assertEqual(self.agent_call('result',result['id'])['status'],'done')
        self.assertEqual(self.agent_call('execute',request)['id'],result['id'])
        self.assertIn('Name:** Pip',self.agent_call('read',{'resource':'document','id':'identity-profile'})['body'])

    def test_136_agent_decline_and_escape_do_not_mutate(self):
        self.agent_enabled()
        before=self.agent_call('read',{'resource':'document','id':'identity-profile'})['body']
        first=self.action('no',{'type':'settings.name','value':'Nope'})
        self.page.get_by_role('button',name='Decline',exact=True).click()
        self.assertEqual(self.agent_call('result',first['id'])['status'],'cancelled')
        second=self.action('escape',{'type':'settings.name','value':'Nope'})
        self.page.keyboard.press('Escape');self.page.wait_for_timeout(30)
        self.assertEqual(self.agent_call('result',second['id'])['status'],'cancelled')
        self.assertEqual(self.agent_call('read',{'resource':'document','id':'identity-profile'})['body'],before)

    def test_137_agent_blocks_human_only_actions_internal_events_and_unknown_fields(self):
        self.agent_enabled()
        for action in [ {'type':'purchase.approve','id':'x','revision':1,'total':20}, {'type':'browser.takeover','id':'x'}, {'type':'permission.network','key':'ssh','value':True}, {'type':'demo.reset'}, {'type':'settings.name','value':'Nope','approve':True} ]:
            result=self.action('blocked-'+str(len(str(action))),action);self.assertEqual(result['status'],'blocked')
        expect(self.page.locator('dialog:modal')).to_have_count(0)

    def test_138_agent_blocks_stale_revision_conflicting_keys_and_modal_navigation(self):
        self.agent_enabled();rev=self.agent_call('inspect')['revision']
        bad=self.agent_call('execute',{'key':'stale','expectedRevision':rev-1,'action':{'type':'settings.name','value':'Old'}});self.assertEqual(bad['status'],'blocked')
        self.action('same',{'type':'ui.side-chats','open':False})
        bad=self.action('same',{'type':'ui.side-chats','open':True});self.assertEqual(bad['status'],'blocked')
        self.action('settings',{'type':'ui.settings','section':'general'})
        self.assertEqual(self.action('interrupt',{'type':'ui.navigate','route':'goals'})['status'],'blocked')
        expect(self.page.get_by_role('dialog',name='Settings',exact=True)).to_be_visible()

    def test_139_agent_schema_and_receipts_are_copies_not_live_mutable_references(self):
        self.agent_enabled()
        self.page.evaluate('''()=>{const a=window.hatchlingFrontend;const l=a.list();l[0].policy='internal';const s=a.inspect();s.chats.length=0;}''')
        self.assertEqual(self.agent_call('list')[0]['policy'],'view');self.assertGreater(len(self.agent_call('inspect')['chats']),0)
        result=self.page.evaluate('''async()=>{const a=window.hatchlingFrontend;const r=await a.execute({key:'copy',action:{type:'ui.side-chats',open:false}});const id=r.id;r.status='rejected';return a.result(id);}''')
        self.assertEqual(result['status'],'done')

    def test_140_agent_review_invalidates_when_live_activity_changes_revision(self):
        self.page.clock.install()
        self.agent_enabled();self.scenario('fix-ci')
        request=self.action('stale-review',{'type':'settings.name','value':'Too late'})
        self.page.clock.fast_forward(2500)
        self.page.get_by_role('button',name='Approve this change',exact=True).click()
        self.assertEqual(self.agent_call('result',request['id'])['status'],'stale')
        self.assertIn('Name:** hatchling',self.agent_call('read',{'resource':'document','id':'identity-profile'})['body'])

    def test_141_agent_control_cannot_be_enabled_by_synthetic_click_and_can_be_revoked(self):
        self.open_settings();self.page.get_by_role('button',name='Help & support',exact=True).click();self.page.get_by_role('button',name='Agent control',exact=True).click()
        self.page.get_by_role('button',name='Enable for this tab',exact=True).evaluate('(el)=>el.click()')
        self.assertFalse(self.agent_call('status')['enabled'])
        self.page.get_by_role('button',name='Enable for this tab',exact=True).click()
        self.assertTrue(self.agent_call('status')['enabled'])
        self.page.get_by_role('button',name='Disable agent control',exact=True).click()
        self.assertFalse(self.agent_call('status')['enabled'])
        self.assertIn('error',self.agent_call('inspect'))

    def test_142_identity_markdown_download_contains_document_not_ui_notice(self):
        dock=self.identity_document();dock.locator('.menu > summary').click()
        with self.page.expect_download() as download:
            dock.get_by_role('button',name='Download Markdown',exact=True).click()
        text=open(download.value.path()).read()
        self.assertIn('# IDENTITY.md',text);self.assertNotIn('About this file',text)
