"""Disposable external-service fixtures. Never import these into production code."""
import argparse
import hashlib
import json
import re
import signal
import subprocess
import threading
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

from github_fixture import GitHub, TOKENS
from external_auth_fixture import ExternalAuth


class Rejected(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


class ScriptedModel:
    def __init__(self, scenario):
        self.scripts = json.loads(scenario.read_text())
        self.scripts.pop('_description', None)
        self.positions = {}
        self.previous_calls = {}
        self.cache = {}

    @staticmethod
    def text(value):
        if isinstance(value, str):
            try:
                return ScriptedModel.text(json.loads(value))
            except (ValueError, TypeError):
                return value
        if isinstance(value, dict):
            for key in ('output', 'text', 'content'):
                if key in value:
                    return ScriptedModel.text(value[key])
            return json.dumps(value, ensure_ascii=False)
        if isinstance(value, list):
            return '\n'.join(ScriptedModel.text(part) for part in value)
        return str(value)

    @staticmethod
    def variables(messages):
        values = {}
        context = '\n'.join(ScriptedModel.text(m.get('content', '')) for m in messages)
        plans = re.findall(r'/home/coder/\.coder/plans/PLAN-[0-9a-f-]{36}\.md', context)
        if plans:
            values['chat_plan'] = plans[-1]
        for message in messages:
            if message.get('role') != 'tool':
                continue
            for line in ScriptedModel.text(message.get('content', '')).splitlines():
                if re.fullmatch(r'[A-Za-z0-9._/-]+/T-[0-9]+', line):
                    values['branch'] = line
                try:
                    item = json.loads(line)
                except ValueError:
                    if '"phase"' in line and '"review_round"' in line:
                        for key in ('revision', 'round', 'head'):
                            values.pop(key, None)
                    continue
                if isinstance(item, dict) and 'phase' in item and 'review_round' in item:
                    values.update(revision=item['revision'], round=item['review_round'], head=item.get('head'))
                    if item.get('pr_number'):
                        values['pr_number'] = item['pr_number']
                if isinstance(item, dict) and 'number' in item and 'head' in item:
                    values['pr_number'] = item['number']
        return values

    def complete(self, body):
        model = body.get('model')
        if model not in self.scripts:
            raise Rejected(400, 'Unknown scenario model')
        messages = body.get('messages')
        if not isinstance(messages, list) or not messages:
            raise Rejected(400, 'Messages are required')
        script = self.scripts[model]
        session = model
        if isinstance(script, dict):
            auxiliary = script.get('auxiliary', [])
            if isinstance(auxiliary, dict):
                auxiliary = [auxiliary]
            for response in auxiliary:
                if any(m.get('role') == 'system' and response['contains'] in
                        self.text(m.get('content', '')) for m in messages):
                    return {'id': 'chatcmpl-auxiliary', 'object': 'chat.completion', 'created': 0,
                        'model': model, 'choices': [{'index': 0, 'message': {
                            'role': 'assistant', 'content': response['text']}, 'finish_reason': 'stop'}],
                        'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}
            users = [(i, m.get('content')) for i, m in enumerate(messages) if m.get('role') == 'user']
            if script.get('turn_contains'):
                users = [(i, content) for i, content in users if any(
                    phrase in self.text(content) for phrase in script['turn_contains'])]
            def matches(route):
                candidates = users[-1:] if script.get('turn_contains') else users
                if 'not_contains' in route:
                    excluded = route['not_contains'] if isinstance(route['not_contains'], list) else [route['not_contains']]
                    if any(phrase in self.text(content) for _, content in candidates for phrase in excluded):
                        return False
                if 'contains_all' in route:
                    phrases = route['contains_all']
                    return any(all(phrase in self.text(content) for phrase in phrases)
                        for _, content in candidates)
                phrases = route['contains'] if isinstance(route['contains'], list) else [route['contains']]
                return any(phrase in self.text(content) for _, content in candidates for phrase in phrases)
            routes = [(i, route) for i, route in enumerate(script['routes'])
                if matches(route)]
            if len(routes) != 1 or not users:
                raise Rejected(400, 'Expected one explicit conversation route')
            route_index, route = routes[0]
            session = hashlib.sha256(json.dumps([model, route_index, users[0], users[-1]],
                sort_keys=True).encode()).hexdigest()[:24]
            script = route['steps']
        fingerprint = hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest()
        if fingerprint in self.cache:
            return self.cache[fingerprint]
        index = self.positions.get(session, 0)
        if index >= len(script):
            raise Rejected(409, 'Scenario exhausted')
        step = script[index]
        previous = self.previous_calls.get(session)
        if previous:
            results = [m for m in messages if m.get('role') == 'tool' and
                m.get('tool_call_id') == previous]
            if len(results) != 1:
                raise Rejected(409, 'Expected the matching tool result')
            required = step.get('requires', {}).get('contains')
            content = results[0].get('content')
            text = self.text(content)
            if required and required not in text:
                raise Rejected(409, 'Tool result did not satisfy the scenario')
        message = {'role': 'assistant', 'content': step.get('text')}
        reason = 'stop'
        if 'tool' in step:
            functions = [tool.get('function', {}) for tool in body.get('tools', [])
                if tool.get('type') == 'function']
            function = next((f for f in functions if f.get('name') == step['tool']), None)
            if function is None:
                raise Rejected(400, 'Scripted tool was not advertised by the caller')
            values = self.variables(messages)
            arguments = {name: re.sub(r'\{(chat_plan|revision|round|head|branch|pr_number)\}',
                lambda match: str(values[match[1]]), value) if isinstance(value, str) else value
                for name, value in step['arguments'].items()}
            schema = function.get('parameters', {})
            properties = schema.get('properties', {})
            if not set(schema.get('required', [])).issubset(arguments):
                raise Rejected(400, 'Scripted arguments omit a required tool parameter')
            types = {'string': str, 'object': dict, 'array': list, 'boolean': bool,
                'integer': int, 'number': (int, float)}
            for name, value in arguments.items():
                if name not in properties:
                    raise Rejected(400, 'Scripted argument is not in the advertised tool schema')
                expected = types.get(properties[name].get('type'))
                if expected and (not isinstance(value, expected) or
                    (isinstance(value, bool) and properties[name].get('type') in ('integer', 'number'))):
                    raise Rejected(400, 'Scripted argument has the wrong type')
            call_id = f'call_{session}_{index}'
            message['tool_calls'] = [{'id': call_id, 'type': 'function', 'function': {
                'name': step['tool'], 'arguments': json.dumps(arguments)}}]
            self.previous_calls[session] = call_id
            reason = 'tool_calls'
        else:
            self.previous_calls.pop(session, None)
        response = {'id': f'chatcmpl-{session}-{index}', 'object': 'chat.completion',
            'created': 0, 'model': model, 'choices': [{'index': 0, 'message': message,
                'finish_reason': reason}],
            'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}
        self.positions[session] = index + 1
        self.cache[fingerprint] = response
        return response


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, service, root, scenario, git_host, git_port, remote_host):
        super().__init__(address, Handler)
        self.service = service
        self.root = root
        self.lock = threading.Lock()
        self.model = ScriptedModel(scenario) if service == 'model' else None
        self.oauth = ExternalAuth(Rejected) if service == 'oauth' else None
        self.github = None
        try:
            if service == 'github':
                self.github = GitHub(root, Rejected, git_host, git_port, remote_host)
        except Exception:
            super().server_close()
            raise
        self.journal = root / f'{service}-requests.jsonl'

    def server_close(self):
        if self.github:
            self.github.repo.close()
        super().server_close()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.handle_request()

    def do_POST(self):
        self.handle_request()

    def do_PUT(self):
        self.handle_request()

    def do_PATCH(self):
        self.handle_request()

    def handle_request(self):
        path = urlsplit(self.path).path
        status = 200
        response = None
        body = {}
        try:
            if path == '/health' and self.command == 'GET':
                self.send_json(200, {'status': 'ready'})
                return
            token = self.headers.get('Authorization', '').removeprefix('Bearer ').removeprefix('token ')
            role = TOKENS.get(token) if self.server.service == 'github' else None
            if (self.server.service == 'model' and token != 'ci-model-key') or (self.server.service == 'github' and not role):
                raise Rejected(401, 'Invalid disposable credential')
            length = int(self.headers.get('Content-Length', '0'))
            if length < 0 or length > 1024 * 1024:
                raise Rejected(413, 'Request exceeds fixture limit')
            body = json.loads(self.rfile.read(length)) if length else {}
            with self.server.lock:
                if self.server.service == 'model' and self.command == 'POST' and path == '/v1/chat/completions':
                    response = self.server.model.complete(body)
                elif self.server.service == 'github':
                    status, response = self.server.github.handle(self.command, self.path, body, role)
                elif self.server.service == 'oauth':
                    status, response = self.server.oauth.handle(self.command, self.path, body, token)
                else:
                    raise Rejected(404, 'Unsupported fixture request')
        except Rejected as error:
            status, response = error.status, {'message': str(error)}
        except (ValueError, KeyError, TypeError) as error:
            status, response = 400, {'message': f'Invalid request: {error}'}
        except subprocess.CalledProcessError as error:
            status, response = 422, {'message': f'Git operation rejected: {error.stderr}'}
        except subprocess.TimeoutExpired:
            status, response = 504, {'message': 'Git operation timed out'}
        finally:
            if response is not None:
                with self.server.lock:
                    with self.server.journal.open('a') as journal:
                        journal.write(json.dumps({'method': self.command, 'path': path,
                            'status': status, 'response': response,
                            'request': body if self.server.service in ('model', 'github') else None}) + '\n')
        if status == 200 and self.server.service == 'model' and body.get('stream'):
            self.send_stream(response)
        else:
            self.send_json(status, response)

    def send_stream(self, response):
        choice = response['choices'][0]
        delta = dict(choice['message'])
        if 'tool_calls' in delta:
            delta['tool_calls'] = [dict(call, index=index)
                for index, call in enumerate(delta['tool_calls'])]
        common = {key: response[key] for key in ('id', 'created', 'model')}
        common['object'] = 'chat.completion.chunk'
        chunks = [dict(common, choices=[{'index': 0, 'delta': delta, 'finish_reason': None}]),
            dict(common, choices=[{'index': 0, 'delta': {}, 'finish_reason': choice['finish_reason']}])]
        encoded = ''.join('data: ' + json.dumps(chunk) + '\n\n' for chunk in chunks)
        encoded = (encoded + 'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def send_json(self, status, body):
        encoded = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def create_server(service, root, scenario=None, host='127.0.0.1', port=0,
        git_host='127.0.0.1', git_port=0, remote_host='127.0.0.1'):
    root.mkdir(parents=True, exist_ok=True)
    return FixtureServer((host, port), service, root, scenario, git_host, git_port, remote_host)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--service', choices=['model', 'github', 'oauth'], required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--scenario', type=Path)
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, default=0)
    parser.add_argument('--advertise-host', default='127.0.0.1')
    parser.add_argument('--ready-file', type=Path, required=True)
    parser.add_argument('--git-host', default='127.0.0.1')
    parser.add_argument('--git-port', type=int, default=0)
    parser.add_argument('--remote-host', default='127.0.0.1')
    args = parser.parse_args()
    if args.service == 'model' and args.scenario is None:
        parser.error('--scenario is required for the model fixture')
    server = create_server(args.service, args.root, args.scenario, args.host, args.port,
        args.git_host, args.git_port, args.remote_host)
    def stop(_signal, _frame):
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        # Existence means readiness: rename a completed file on the same filesystem.
        with tempfile.NamedTemporaryFile(mode='w', dir=args.ready_file.parent,
                prefix=args.ready_file.name + '.', delete=False) as ready:
            pending = Path(ready.name)
            json.dump({'url': f'http://{args.advertise_host}:{server.server_port}'}, ready)
        try:
            pending.replace(args.ready_file)
        finally:
            pending.unlink(missing_ok=True)
        server.serve_forever()
    finally:
        server.server_close()
