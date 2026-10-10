# Tests the bootstrap driver's diagnostic behavior when a chat cannot complete.
# Runs a local HTTP server with the Coder endpoints used by the driver.
# Sends actual HTTP requests through the driver's normal request helper.
# Returns an explicit failed chat and its failed command result.
# Checks that the original chat error still reaches the test caller.
# Checks that chat state and messages are saved before teardown.
# Exercises the timeout path without waiting for the full scenario deadline.
# Exercises unavailable state and message endpoints independently.
# Keeps diagnostic files and server state inside a temporary directory.
# These are driver tests; the container scenario exercises actual Coder.
# No production server, account, workspace, or credentials are used here.

import importlib.util
import json
import os
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch


class BootstrapDiagnosticsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.state = {'status': 'error'}
        self.messages = {'messages': [{'role': 'tool', 'content': [
            {'type': 'tool-result', 'tool_name': 'execute', 'is_error': True,
             'result': {'exit_code': 1, 'output': 'canary command failed'}}]}]}
        self.unavailable = set()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path == '/api/v2/chats/test-chat':
                    data = fixture.state
                elif self.path == '/api/v2/chats/test-chat/messages?limit=100':
                    data = fixture.messages
                else:
                    self.send_error(404)
                    return
                if self.path in fixture.unavailable:
                    self.send_error(503)
                    return
                body = json.dumps(data).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        spec = importlib.util.spec_from_file_location('bootstrap_under_test',
            Path(__file__).with_name('production_bootstrap.py'))
        self.driver = importlib.util.module_from_spec(spec)
        with patch.dict(os.environ, {
            'OPENFLOWS_E2E_ARTIFACTS': str(self.root),
            'CODER_URL': f'http://127.0.0.1:{self.server.server_port}',
            'OPENFLOWS_E2E_TENANT': 'test-tenant',
        }):
            spec.loader.exec_module(self.driver)

    def assert_saved_chat(self):
        self.assertEqual(json.loads((self.root / 'chat-state.json').read_text()), self.state)
        self.assertEqual(json.loads((self.root / 'chat.json').read_text()), self.messages)

    def test_chat_error_preserves_state_and_failed_tool_result(self):
        with self.assertRaisesRegex(RuntimeError, 'Real Coder chat failed'):
            self.driver.wait_for_chat('test-chat')
        self.assert_saved_chat()

    def test_chat_timeout_preserves_state_and_messages(self):
        self.state['status'] = 'running'
        with self.assertRaises(TimeoutError):
            self.driver.wait_for_chat('test-chat', seconds=0)
        self.assert_saved_chat()

    def test_unavailable_state_does_not_hide_timeout_or_prevent_message_capture(self):
        self.unavailable.add('/api/v2/chats/test-chat')
        with self.assertRaises(TimeoutError):
            self.driver.wait_for_chat('test-chat', seconds=0)
        self.assertEqual(json.loads((self.root / 'chat.json').read_text()), self.messages)
        errors = json.loads((self.root / 'chat-diagnostics-errors.json').read_text())
        self.assertIn('HTTP 503', errors['chat-state.json'])

    def test_unavailable_messages_do_not_hide_the_original_chat_error(self):
        self.unavailable.add('/api/v2/chats/test-chat/messages?limit=100')
        with self.assertRaisesRegex(RuntimeError, 'Real Coder chat failed'):
            self.driver.wait_for_chat('test-chat')
        self.assertEqual(json.loads((self.root / 'chat-state.json').read_text()), self.state)
        errors = json.loads((self.root / 'chat-diagnostics-errors.json').read_text())
        self.assertIn('HTTP 503', errors['chat.json'])


if __name__ == '__main__':
    unittest.main()
