"""Actual loopback CDP + newline JSON-RPC/stdio integration. No external accounts."""
from __future__ import annotations
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest
from playwright.sync_api import sync_playwright

ROOT=Path(__file__).resolve().parents[2]
TOOL=ROOT/'tools/agent.py'
spec=importlib.util.spec_from_file_location('agent_companion',TOOL)
companion=importlib.util.module_from_spec(spec);spec.loader.exec_module(companion)

class CompanionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile=tempfile.TemporaryDirectory(prefix='hatchling-frontend-test-')
        with socket.socket() as s:
            s.bind(('127.0.0.1',0));port=s.getsockname()[1]
        cls.endpoint=f'http://127.0.0.1:{port}'
        cls.driver=sync_playwright().start()
        cls.process=subprocess.Popen([os.environ.get('CHROMIUM_PATH') or shutil.which('chromium') or cls.driver.chromium.executable_path,'--headless','--no-sandbox','--disable-dev-shm-usage',f'--remote-debugging-port={port}','--remote-debugging-address=127.0.0.1',f'--user-data-dir={cls.profile.name}','about:blank'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        for _ in range(25):
            try: cls.browser=cls.driver.chromium.connect_over_cdp(cls.endpoint,timeout=1000);break
            except Exception: time.sleep(.2)
        else: raise RuntimeError('Dedicated local debug browser did not start.')
        cls.context=cls.browser.contexts[0]
        cls.page=cls.context.pages[0]
        cls.page.set_content((ROOT/'dist/hatchling-standalone.html').read_text(),wait_until='domcontentloaded')
        cls.page.locator('#message-input').wait_for()

    @classmethod
    def tearDownClass(cls):
        cls.browser.close();cls.driver.stop();cls.process.terminate();cls.process.wait(timeout=10)
        # Chromium's helpers can still be writing the profile as they exit.
        for _ in range(20):
            try: cls.profile.cleanup();break
            except OSError: time.sleep(.25)
        else: cls.profile.cleanup()

    def cli(self,method,args=None):
        result=subprocess.run(['python3',str(TOOL),'--cdp',self.endpoint,method,json.dumps(args or {})],capture_output=True,text=True,timeout=15)
        return result.returncode,json.loads(result.stdout or result.stderr)

    def mcp(self,messages):
        proc=subprocess.run(['python3',str(TOOL),'--cdp',self.endpoint,'--mcp'],input='\n'.join(json.dumps(x) for x in messages)+'\n',capture_output=True,text=True,timeout=20)
        self.assertEqual(proc.returncode,0,proc.stderr)
        return [json.loads(line) for line in proc.stdout.splitlines()]

    def init(self):
        return [{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'test','version':'1'}}},{'jsonrpc':'2.0','method':'notifications/initialized'}]

    def test_01_disabled_cli_and_explicit_local_targets(self):
        code,status=self.cli('status');self.assertEqual(code,0);self.assertFalse(status['enabled'])
        code,data=self.cli('inspect');self.assertEqual(code,1);self.assertIn('error',data)
        for url in [None,'https://example.com','http://0.0.0.0:9222','http://user:pw@localhost:9222','file:///tmp/test']:
            with self.assertRaises(ValueError):companion.validate_target(url)

    def test_02_mcp_init_list_and_disabled_content_read(self):
        data=self.mcp(self.init()+[{'jsonrpc':'2.0','id':2,'method':'tools/list'},{'jsonrpc':'2.0','id':3,'method':'tools/call','params':{'name':'hatchling_frontend_read','arguments':{'resource':'document','id':'identity-profile'}}}])
        self.assertEqual(data[0]['result']['protocolVersion'],'2025-11-25')
        self.assertEqual(len(data[1]['result']['tools']),6)
        self.assertTrue(data[2]['result']['isError'])

    def test_03_enabled_cli_and_mcp_share_the_actual_ui(self):
        self.page.keyboard.press('Control+,')
        self.page.get_by_role('button',name='Help & support',exact=True).click()
        self.page.get_by_role('button',name='Agent control',exact=True).click()
        self.page.get_by_role('button',name='Enable for this tab',exact=True).click()
        self.page.get_by_role('button',name='Close Agent control',exact=True).click()
        self.page.get_by_role('button',name='Close Settings',exact=True).click()
        code,state=self.cli('inspect');self.assertEqual(code,0);self.assertEqual(state['product'],'hatchling-frontend')
        code,result=self.cli('execute',{'key':'cli-goals','action':{'type':'ui.navigate','route':'goals'}});self.assertEqual(result['status'],'done')
        self.page.get_by_role('heading',name='Goals',level=1).wait_for()
        messages=self.init()+[{'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'hatchling_frontend_execute','arguments':{'key':'mcp-identity','action':{'type':'ui.document','id':'identity-profile'}}}}]
        data=self.mcp(messages);self.assertFalse(data[-1]['result']['isError']);self.assertEqual(json.loads(data[-1]['result']['content'][0]['text'])['status'],'done')
        self.page.locator('.identity-document-dock').wait_for()

    def test_04_cli_mutation_is_pending_and_decline_is_a_real_result(self):
        self.page.keyboard.press('Escape')
        _,state=self.cli('inspect')
        code,result=self.cli('execute',{'key':'cli-review','expectedRevision':state['revision'],'action':{'type':'settings.name','value':'Must be reviewed'}})
        self.assertEqual(code,0);self.assertEqual(result['status'],'pending')
        self.page.get_by_role('button',name='Decline',exact=True).click()
        _,outcome=self.cli('result',{'id':result['id']});self.assertEqual(outcome['status'],'cancelled')

    def test_05_invalid_initialization_and_tool_arguments_fail_closed(self):
        results=self.mcp([{'jsonrpc':'2.0','id':1,'method':'tools/list'}]+self.init()+[{'jsonrpc':'2.0','id':3,'method':'tools/call','params':{'name':'hatchling_frontend_execute','arguments':{'key':12,'action':{}}}}])
        self.assertIn('error',results[0]);self.assertTrue(results[-1]['result']['isError'])

    def test_06_second_frontend_tab_requires_explicit_selection(self):
        extra=self.context.new_page();extra.set_content((ROOT/'dist/hatchling-standalone.html').read_text());extra.locator('#message-input').wait_for()
        code,result=self.cli('inspect');self.assertEqual(code,1);self.assertIn('exactly one',result['error']);extra.close()

if __name__=='__main__':unittest.main(verbosity=2)
