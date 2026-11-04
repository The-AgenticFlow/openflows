"""Exercise the public tenant CLI, production Terraform/bootstrap, and real Coder chat.

This tracer establishes workspace startup; it is not the full issue-to-merge test.
"""
import base64
import json
import os
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path


ARTIFACTS = Path(os.environ['OPENFLOWS_E2E_ARTIFACTS'])
CODER = os.environ['CODER_URL']
TENANT = os.environ['OPENFLOWS_E2E_TENANT']
TOKEN = None
TENANT_LINKED = False  # The link persists across `tenant add` retries.


def request(base, path, body=None, token=None, coder=False):
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Coder-Session-Token' if coder else 'Authorization'] = token if coder else f'Bearer {token}'
    req = urllib.request.Request(base + path,
        data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise RuntimeError(f'{req.get_method()} {path}: HTTP {error.code}: {error.read().decode()}') from error


def coder(path, body=None):
    return request(CODER, '/api/v2' + path, body, TOKEN, coder=True)


def wait(description, predicate, seconds=180):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(2)
    raise TimeoutError(description)


def ssh(name, command):
    encoded = base64.b64encode(command.encode()).decode()
    return subprocess.run(['coder', 'ssh', name, '--', f'echo {encoded} | base64 -d | bash -l'],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=40, check=True).stdout


def save_chat_diagnostics(chat_id):
    errors = {}
    for filename, endpoint in (
        ('chat-state.json', '/chats/' + chat_id),
        ('chat.json', '/chats/' + chat_id + '/messages?limit=100'),
    ):
        try:
            (ARTIFACTS / filename).write_text(json.dumps(coder(endpoint), indent=2))
        except Exception as error:
            # Collect each endpoint independently and preserve the scenario failure.
            errors[filename] = str(error)
    if errors:
        (ARTIFACTS / 'chat-diagnostics-errors.json').write_text(json.dumps(errors, indent=2))


def wait_for_chat(chat_id, seconds=180):
    def settled():
        current = coder('/chats/' + chat_id)
        if current['status'] == 'error':
            raise RuntimeError(f'Real Coder chat failed: {current}')
        messages = coder('/chats/' + chat_id + '/messages?limit=100')
        completed = any(part.get('text') == 'The production workspace executed the bootstrap canary.'
            for message in messages['messages'] if message['role'] == 'assistant'
            for part in message.get('content', []))
        return messages if completed and current['status'] == 'waiting' else False
    try:
        messages = wait('real Coder chat did not complete the tool-result conversation', settled, seconds)
        (ARTIFACTS / 'chat.json').write_text(json.dumps(messages, indent=2))
        return messages
    finally:
        save_chat_diagnostics(chat_id)


def link_github(token):
    """Link the `primary-github` external auth for the user behind `token`."""
    def call(path, body=None):
        return request(CODER, '/api/v2' + path, body, token, coder=True)
    device = call('/external-auth/primary-github/device')
    request(os.environ['OPENFLOWS_E2E_OAUTH_URL'], '/approve',
        {'user_code': device['user_code']}, 'ci-operator-token')
    call('/external-auth/primary-github/device', {'device_code': device['device_code']})
    assert call('/external-auth/primary-github')['authenticated'] is True


def link_tenant_github():
    """Once `tenant add` has created the tenant's own Coder user, link GitHub as
    that tenant. Each tenant links its own account; the operator's link is not
    shared. Returns False while the tenant user does not exist yet."""
    users = coder('/users')['users']
    tenant = next((u for u in users if u['username'] != 'ci'
        and u['email'].endswith('@openflows.local')), None)
    if tenant is None:
        return False
    key = coder('/users/' + tenant['id'] + '/keys/tokens',
        {'token_name': 'e2e-github-link', 'lifetime': 3600 * 10**9})['key']
    link_github(key)
    print(f"Tenant Coder user '{tenant['username']}' linked its own GitHub account.", flush=True)
    return True


def run_tenant_add(log):
    """Run `openflows tenant add`, linking the tenant's GitHub account while the
    CLI waits for it. Returns the CLI exit code."""
    proc = subprocess.Popen(['openflows', 'tenant', 'add', 'test/repo', '--name', TENANT, '--fleet', '1'],
        stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
    global TENANT_LINKED
    deadline = time.monotonic() + 600
    try:
        while proc.poll() is None:
            if time.monotonic() > deadline:
                raise TimeoutError('openflows tenant add did not finish')
            if not TENANT_LINKED:
                TENANT_LINKED = link_tenant_github()
            time.sleep(2)
    finally:
        # Never leave provisioning running after a failed link or timeout: the
        # launcher tears down Coder as soon as this test exits.
        if proc.poll() is None:
            proc.kill()
        proc.wait()
    return proc.returncode


def setup_tenant(model_name='bootstrap-canary'):
    global TOKEN
    def server_ready():
        try:
            return coder('/buildinfo')
        except (RuntimeError, urllib.error.URLError, TimeoutError):
            return False
    wait('Coder HTTP server did not become ready', server_ready)
    credentials = {'email': 'ci@example.test', 'username': 'ci',
        'password': 'Disposable-CI-password-729!', 'trial': False}
    coder('/users/first', credentials)  # Fails if this is an existing deployment.
    TOKEN = coder('/users/login', credentials)['session_token']
    os.environ['CODER_SESSION_TOKEN'] = TOKEN
    link_github(TOKEN)
    print('Disposable Coder account linked through approved device flow.', flush=True)
    provider = coder('/ai/providers', {'type': 'openai-compat', 'name': 'ci-model',
        'base_url': 'http://model:8080/v1', 'enabled': True, 'api_keys': ['ci-model-key']})
    organization = next(o for o in coder('/users/me/organizations') if o['is_default'])
    coder(f"/organizations/{organization['id']}/chats/models", {
        'ai_provider_id': provider['id'], 'model': model_name,
        'context_limit': 128000, 'is_default': True})
    with (ARTIFACTS / 'tenant-add.log').open('w') as log:
        print('Running the public tenant CLI with production templates.', flush=True)
        for attempt in range(1, 4):
            returncode = run_tenant_add(log)
            if returncode == 0:
                break
            if attempt == 3:
                raise subprocess.CalledProcessError(returncode, ['openflows', 'tenant', 'add'])
            print(f'openflows tenant add attempt {attempt} exited with {returncode}; retrying after transient error...', flush=True)
            time.sleep(5)
    templates = coder('/organizations/' + organization['id'] + '/templates')
    assert {t['name'] for t in templates} == {
        'openflows-nexus', 'openflows-forge', 'openflows-sentinel', 'openflows-vessel', 'openflows-lore'}
    workspace = next(w for w in coder('/workspaces')['workspaces'] if w['name'] == 'openflows-nexus-' + TENANT)
    parameters = {p['name']: p['value'] for p in coder('/workspacebuilds/' + workspace['latest_build']['id'] + '/parameters')}
    fleet = json.loads(parameters['registry_json'])
    assert {a['id']: a['max_instances'] for a in fleet['team'] if a['id'] in ('forge', 'sentinel')} == {'forge': 1, 'sentinel': 1}
    assert parameters['start_controller'] == 'true'
    # The Nexus workspace belongs to the tenant's own Coder user, not `ci`.
    name = workspace['owner_name'] + '/' + workspace['name']
    def ready():
        try:
            return ssh(name, "set -eu; curl -fsS http://localhost:3001/experimental/hooks/health; test -d .git; test \"$(cat answer.txt)\" = 41")
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            return False
    wait('production Nexus controller/bootstrap did not become ready', ready)
    print('Production Nexus workspace cloned the repository and started its controller.', flush=True)
    return workspace, templates, organization, name


def run():
    workspace, templates, organization, name = setup_tenant()
    chat = coder('/chats', {'organization_id': organization['id'], 'workspace_id': workspace['id'],
        'content': [{'type': 'text', 'text': 'Execute the production bootstrap canary.'}]})
    messages = wait_for_chat(chat['id'])
    results = [part for message in messages['messages'] for part in message.get('content', [])
        if part['type'] == 'tool-result' and part.get('tool_name') == 'execute']
    assert len(results) == 1 and not results[0].get('is_error'), 'No successful real workspace execution'
    result = results[0]['result']
    if isinstance(result, str):
        result = json.loads(result)
    assert result['exit_code'] == 0 and 'PRODUCTION_BOOTSTRAP_OK' in result['output']
    print('Real Coder chat executed the workspace command and returned a successful tool result.', flush=True)
    (ARTIFACTS / 'controller.log').write_text(ssh(name, 'cat /tmp/openflows-controller.log'))
    for resource in workspace['latest_build']['resources']:
        for agent in resource.get('agents') or []:
            logs = coder('/workspaceagents/' + agent['id'] + '/logs?after=0')
            (ARTIFACTS / ('agent-' + agent['id'] + '.json')).write_text(json.dumps(logs, indent=2))
    before = {t['name']: t['active_version_id'] for t in templates}
    def bootstrap(log_name):
        with (ARTIFACTS / log_name).open('w') as log:
            subprocess.run(['openflows', 'bootstrap'], stdin=subprocess.DEVNULL,
                stdout=log, stderr=subprocess.STDOUT, check=True, timeout=300)
        return {t['name']: t['active_version_id'] for t in coder('/organizations/' + organization['id'] + '/templates')}
    assert bootstrap('bootstrap-repeat.log') == before, 'Unchanged bootstrap must reuse template versions'
    print('Unchanged bootstrap reused all five template versions; checking changed configuration.', flush=True)
    os.environ['TF_VAR_github_api_base'] += '/'
    changed = bootstrap('bootstrap-config-change.log')
    assert all(changed[name] != version for name, version in before.items()), 'Changed template variables were ignored by bootstrap'
    stored_variables = {}
    for template_name, version in changed.items():
        variables = coder('/templateversions/' + version + '/variables')
        stored_variables[template_name] = {v['name']: v['value'] for v in variables if not v.get('sensitive')}
    (ARTIFACTS / 'template-variables.json').write_text(json.dumps(stored_variables, indent=2))
    for template_name, variables in stored_variables.items():
        assert variables['github_api_base'] == os.environ['TF_VAR_github_api_base'], \
            f'{template_name} did not store the updated GitHub API setting: {variables}'
    print('All five fresh template versions stored the changed Terraform configuration.', flush=True)
    (ARTIFACTS / 'bootstrap-evidence.json').write_text(json.dumps({
        'workspace': workspace, 'templates': templates, 'chat_id': chat['id'],
        'git_clone': 'real git daemon', 'controller_health': 'healthy',
        'full_issue_to_merge': False}, indent=2))
    coder('/workspaces/' + workspace['id'] + '/builds', {'transition': 'delete'})
    def deleted():
        remaining = next((w for w in coder('/workspaces')['workspaces'] if w['id'] == workspace['id']), None)
        if remaining is None:
            return True
        if remaining['latest_build']['status'] in ('failed', 'canceled'):
            raise RuntimeError(f'Production workspace deletion failed: {remaining["latest_build"]}')
        return False
    wait('Coder did not delete the production workspace', deleted)
    print('Production tenant CLI, five templates, Nexus controller, real chat execution and deletion passed.', flush=True)


if __name__ == '__main__':
    run()
