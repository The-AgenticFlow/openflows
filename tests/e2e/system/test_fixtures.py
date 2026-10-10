"""Exercise fixture HTTP boundaries and real Git/process side effects."""
import json
import os
import shutil
import tempfile
import subprocess
import time
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

from fixtures import create_server


class FixtureCase(unittest.TestCase):
    def start(self, service, scenario=None):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self.save_artifacts)
        self.server = create_server(service, self.root, scenario)
        self.addCleanup(self.server.server_close)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.thread.join, 5)
        self.addCleanup(self.server.shutdown)
        self.url = f'http://127.0.0.1:{self.server.server_port}'

    def save_artifacts(self):
        destination = os.environ.get('OPENFLOWS_FIXTURE_ARTIFACTS')
        if not destination:
            return
        target = Path(destination) / self.id().split('.')[-1]
        target.mkdir(parents=True, exist_ok=True)
        for path in self.root.glob('*.jsonl'):
            shutil.copy2(path, target / path.name)
        for path in self.root.glob('*.log'):
            shutil.copy2(path, target / path.name)
        if (self.root / 'ci-artifacts').exists():
            shutil.copytree(self.root / 'ci-artifacts', target / 'ci', dirs_exist_ok=True)


class FixtureTest(FixtureCase):
    def setUp(self):
        with tempfile.TemporaryDirectory() as directory:
            scenario = Path(directory) / 'scenario.json'
            scenario.write_text(json.dumps({'e2e-forge': [
                {'tool': 'execute', 'arguments': {'command': './acceptance.sh'}},
                {'requires': {'contains': 'exit_code=1\n'}, 'text': 'Repair is required.'},
            ], 'e2e-unicode': [
                {'tool': 'execute', 'arguments': {'command': 'inspect'}},
                {'requires': {'contains': 'échec\nréessayer'}, 'text': 'Understood.'},
            ], 'e2e-structured': [
                {'tool': 'execute', 'arguments': {'command': 'inspect'}},
                {'requires': {'contains': '"exit_code": 1'}, 'text': 'Understood.'},
            ]}))
            self.start('model', scenario)

    def request(self, path, body, token='ci-model-key'):
        request = urllib.request.Request(self.url + path, json.dumps(body).encode(),
            {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status, response.read().decode(), response.headers
        except urllib.error.HTTPError as error:
            return error.code, error.read().decode(), error.headers

    def test_model_waits_for_the_matching_real_tool_result_and_retries_are_stable(self):
        request = {'model': 'e2e-forge', 'messages': [{'role': 'user', 'content': 'Verify'}],
            'tools': [{'type': 'function', 'function': {'name': 'execute', 'parameters': {
                'type': 'object', 'properties': {'command': {'type': 'string'}},
                'required': ['command']}}}]}
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200)
        call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        self.assertEqual(call['function']['name'], 'execute')
        self.assertEqual(json.loads(call['function']['arguments']), {'command': './acceptance.sh'})
        (self.root / 'answer.txt').write_text('41\n')
        oracle = self.root / 'acceptance.sh'
        shutil.copy2(Path(__file__).with_name('acceptance.sh'), oracle)
        oracle.chmod(0o700)
        result = subprocess.run(['sh', '-c', json.loads(call['function']['arguments'])['command']],
            cwd=self.root, capture_output=True, text=True, timeout=5,
            env={'PATH': os.defpath, 'HOME': str(self.root), 'LANG': 'C'})
        self.assertEqual(result.returncode, 1)
        tool_output = f'exit_code={result.returncode}\n{result.stdout}{result.stderr}'
        self.assertEqual(self.request('/v1/chat/completions', request)[1], body)
        request['messages'].append({'role': 'assistant', 'tool_calls': [call]})
        self.assertEqual(self.request('/v1/chat/completions', request)[0], 409)
        request['messages'].append({'role': 'tool', 'tool_call_id': 'unrelated', 'content': tool_output})
        self.assertEqual(self.request('/v1/chat/completions', request)[0], 409)
        request['messages'][-1]['tool_call_id'] = call['id']
        request['messages'][-1]['content'] = 'exit_code=0'
        self.assertEqual(self.request('/v1/chat/completions', request)[0], 409)
        request['messages'][-1]['content'] = tool_output
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(body)['choices'][0]['message']['content'], 'Repair is required.')

    def test_streaming_tool_calls_use_the_advertised_schema_and_fail_closed(self):
        request = {'model': 'e2e-forge', 'stream': True,
            'messages': [{'role': 'user', 'content': 'Verify'}], 'tools': []}
        self.assertEqual(self.request('/v1/chat/completions', request)[0], 400)
        request['tools'] = [{'type': 'function', 'function': {'name': 'execute', 'parameters': {
            'type': 'object', 'properties': {'command': {'type': 'integer'}},
            'required': ['command']}}}]
        self.assertEqual(self.request('/v1/chat/completions', request)[0], 400)
        request['tools'][0]['function']['parameters']['properties']['command']['type'] = 'string'
        code, body, headers = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200)
        self.assertEqual(headers['Content-Type'], 'text/event-stream')
        events = [line[6:] for line in body.splitlines() if line.startswith('data: ')]
        self.assertEqual(events[-1], '[DONE]')
        chunks = [json.loads(event) for event in events[:-1]]
        call = chunks[0]['choices'][0]['delta']['tool_calls'][0]
        self.assertEqual(call['index'], 0)
        self.assertEqual(call['function']['name'], 'execute')
        self.assertEqual(chunks[-1]['choices'][0]['finish_reason'], 'tool_calls')
        self.assertEqual(self.request('/v1/chat/completions', request, 'wrong-key')[0], 401)
        self.assertEqual(self.request('/v1/responses', request)[0], 404)

    def test_unicode_and_structured_tool_results_match_their_actual_content(self):
        for model, content in [('e2e-unicode', 'échec\nréessayer'),
                ('e2e-structured', {'exit_code': 1, 'stdout': 'Failed'})]:
            with self.subTest(model=model):
                request = {'model': model, 'messages': [{'role': 'user', 'content': 'Inspect'}],
                    'tools': [{'type': 'function', 'function': {'name': 'execute', 'parameters': {
                        'type': 'object', 'properties': {'command': {'type': 'string'}},
                        'required': ['command']}}}]}
                code, body, _ = self.request('/v1/chat/completions', request)
                self.assertEqual(code, 200)
                call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
                request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
                    {'role': 'tool', 'tool_call_id': call['id'], 'content': content}])
                code, body, _ = self.request('/v1/chat/completions', request)
                self.assertEqual(code, 200)
                self.assertEqual(json.loads(body)['choices'][0]['message']['content'], 'Understood.')


class FixtureCliTest(unittest.TestCase):
    def test_first_visible_ready_file_is_valid_and_advertises_the_configured_host(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scenario = root / 'scenario.json'
            scenario.write_text(json.dumps({'e2e-forge': [{'text': 'Ready.'}]}))
            for advertised in ['127.0.0.1', 'model-fixture']:
                with self.subTest(advertised=advertised):
                    ready = root / f'{advertised}.json'
                    process = subprocess.Popen(['python3', str(Path(__file__).with_name('fixtures.py')),
                        '--service', 'model', '--root', str(root / advertised),
                        '--scenario', str(scenario), '--host', '0.0.0.0',
                        '--advertise-host', advertised, '--ready-file', str(ready)],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    try:
                        deadline = time.monotonic() + 10
                        while not ready.exists():
                            self.assertIsNone(process.poll(), 'Fixture exited before readiness')
                            self.assertLess(time.monotonic(), deadline, 'Fixture startup timed out')
                            time.sleep(0.001)
                        metadata = json.loads(ready.read_text())
                        self.assertTrue(metadata['url'].startswith(f'http://{advertised}:'))
                        port = metadata['url'].rsplit(':', 1)[1]
                        with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=5) as response:
                            self.assertEqual(json.load(response), {'status': 'ready'})
                        self.assertEqual(list(root.glob(ready.name + '.*')), [])
                    finally:
                        process.terminate()
                        try:
                            output, error = process.communicate(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            output, error = process.communicate(timeout=5)
                        self.assertEqual(process.returncode, 0, output + error)


class GitHubFixtureTest(FixtureCase):
    def setUp(self):
        self.start('github')
        _, repository = self.request('GET', '/repos/test/repo')
        self.remote = repository['clone_url']
        self.checkout = self.root / 'forge'
        self.git('clone', self.remote, str(self.checkout))
        self.git('config', 'user.name', 'FORGE')
        self.git('config', 'user.email', 'forge@example.test')
        self.git('switch', '-c', 'fix/T-1')
        self.git('commit', '--allow-empty', '-m', 'broken candidate')
        self.git('push', 'origin', 'HEAD')
        self.broken = self.git('rev-parse', 'HEAD').strip()
        _, pr = self.request('POST', '/repos/test/repo/pulls',
            {'title': 'Fix T-1', 'head': 'fix/T-1', 'base': 'main', 'body': 'Fixes #1'})
        self.pr = pr['number']

    def git(self, *args, cwd=None):
        result = subprocess.run(['git', *args], cwd=cwd or
            (self.checkout if self.checkout.exists() else self.root),
            env=dict(os.environ, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
                GIT_TERMINAL_PROMPT='0'),
            text=True, capture_output=True, check=True, timeout=15)
        return result.stdout

    def request(self, method, path, body=None, token='ci-forge-token'):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(self.url + path, data,
            {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'}, method=method)
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def ci(self, sha):
        result = subprocess.run(['python3', str(Path(__file__).with_name('run_ci.py')),
            '--api', self.url, '--remote', self.remote, '--sha', sha,
            '--oracle', str(Path(__file__).with_name('acceptance.sh')),
            '--artifacts', str(self.root / 'ci-artifacts')],
            capture_output=True, text=True, timeout=30)
        self.assertIn(result.returncode, (0, 1), result.stderr)
        evidence_path = self.root / 'ci-artifacts' / f'{sha}.json'
        self.assertTrue(evidence_path.exists(), result.stderr)
        evidence = json.loads(evidence_path.read_text())
        self.assertEqual(evidence['head_sha'], sha)
        self.assertEqual(evidence['exit_code'] == 0, result.returncode == 0)
        return result.returncode

    def review(self, sha):
        code, _ = self.request('POST', f'/repos/test/repo/pulls/{self.pr}/reviews',
            {'event': 'APPROVE', 'body': 'Verified', 'commit_id': sha, 'comments': []},
            token='ci-sentinel-token')
        self.assertEqual(code, 201)

    def merge(self, sha):
        return self.request('PUT', f'/repos/test/repo/pulls/{self.pr}/merge',
            {'sha': sha, 'merge_method': 'merge', 'commit_title': 'Merge tested change'},
            token='ci-vessel-token')

    def test_failed_acceptance_and_stale_head_cannot_merge_but_verified_fix_really_merges(self):
        self.review(self.broken)
        self.assertEqual(self.ci(self.broken), 1)
        code, checks = self.request('GET', f'/repos/test/repo/commits/{self.broken}/check-runs')
        self.assertEqual(checks['check_runs'][0]['conclusion'], 'failure')
        self.assertEqual(self.merge(self.broken)[0], 405)
        (self.checkout / 'answer.txt').write_text('42\n')
        self.git('add', 'answer.txt')
        self.git('commit', '-m', 'fix answer')
        self.git('push', 'origin', 'HEAD')
        fixed = self.git('rev-parse', 'HEAD').strip()
        self.assertEqual(self.merge(self.broken)[0], 409)
        self.assertEqual(self.merge(fixed)[0], 405)
        self.assertEqual(self.ci(fixed), 0)
        self.assertEqual(self.merge(fixed)[0], 405)  # Old review cannot approve new head.
        self.review(fixed)
        code, result = self.merge(fixed)
        self.assertEqual(code, 200)
        self.assertTrue(result['merged'])
        self.git('fetch', 'origin', 'main')
        self.assertEqual(self.git('show', 'origin/main:answer.txt'), '42\n')
        self.assertEqual(self.git('rev-parse', 'origin/main').strip(), result['sha'])
        self.git('merge-base', '--is-ancestor', fixed, 'origin/main')
        _, pr = self.request('GET', f'/repos/test/repo/pulls/{self.pr}')
        self.assertEqual(pr['merge_commit_sha'], result['sha'])
        self.assertTrue(pr['merged'])
        self.assertEqual(self.ci(result['sha']), 0)  # Acceptance also passes on the actual merged commit.

    def test_only_independent_ci_can_publish_checks_and_candidate_cannot_replace_the_oracle(self):
        self.review(self.broken)
        self.assertEqual(self.merge(self.broken)[0], 405)
        code, _ = self.request('POST', '/repos/test/repo/check-runs',
            {'name': 'acceptance', 'head_sha': self.broken, 'status': 'completed',
                'conclusion': 'success'})
        self.assertEqual(code, 403)
        (self.checkout / 'acceptance.sh').write_text('#!/bin/sh\nexit 0\n')
        self.git('add', 'acceptance.sh')
        self.git('commit', '-m', 'try replacing acceptance')
        self.git('push', 'origin', 'HEAD')
        malicious = self.git('rev-parse', 'HEAD').strip()
        self.assertEqual(self.ci(malicious), 1)
        self.review(malicious)
        self.assertEqual(self.merge(malicious)[0], 405)
        self.assertEqual(self.request('GET', '/repos/test/repo/not-implemented')[0], 404)
        self.assertEqual(self.request('GET', '/user', token='unknown')[0], 401)

    def test_successful_check_on_old_head_cannot_authorize_a_new_commit(self):
        (self.checkout / 'answer.txt').write_text('42\n')
        self.git('add', 'answer.txt')
        self.git('commit', '-m', 'fix answer')
        self.git('push', 'origin', 'HEAD')
        fixed = self.git('rev-parse', 'HEAD').strip()
        self.assertEqual(self.ci(fixed), 0)
        self.review(fixed)
        self.git('commit', '--allow-empty', '-m', 'new candidate identity')
        self.git('push', 'origin', 'HEAD')
        new_head = self.git('rev-parse', 'HEAD').strip()
        self.review(new_head)
        # Current review alone cannot borrow the old commit's successful CI.
        self.assertEqual(self.merge(new_head)[0], 405)
        self.assertEqual(self.ci(new_head), 0)
        self.assertEqual(self.merge(new_head)[0], 200)

    def test_later_request_changes_blocks_merge_and_conflicts_leave_main_unchanged(self):
        (self.checkout / 'answer.txt').write_text('42\n')
        self.git('add', 'answer.txt')
        self.git('commit', '-m', 'fix answer')
        self.git('push', 'origin', 'HEAD')
        fixed = self.git('rev-parse', 'HEAD').strip()
        self.assertEqual(self.ci(fixed), 0)
        self.review(fixed)
        code, _ = self.request('POST', f'/repos/test/repo/pulls/{self.pr}/reviews',
            {'event': 'REQUEST_CHANGES', 'body': 'Needs rework', 'commit_id': fixed},
            token='ci-sentinel-token')
        self.assertEqual(code, 201)
        self.assertEqual(self.merge(fixed)[0], 405)
        self.review(fixed)
        self.git('switch', 'main')
        (self.checkout / 'answer.txt').write_text('43\n')
        self.git('add', 'answer.txt')
        self.git('commit', '-m', 'concurrent conflicting base change')
        self.git('push', 'origin', 'main')
        base = self.git('rev-parse', 'HEAD').strip()
        self.assertEqual(self.merge(fixed)[0], 422)
        self.git('fetch', 'origin', 'main')
        self.assertEqual(self.git('rev-parse', 'origin/main').strip(), base)
        _, pr = self.request('GET', f'/repos/test/repo/pulls/{self.pr}')
        self.assertFalse(pr['merged'])


if __name__ == '__main__':
    unittest.main()
