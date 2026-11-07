"""Observe the real controller journey; only the human and independent CI are driven here."""
import json
import os
import re
import subprocess
from pathlib import Path

import production_bootstrap as bootstrap


ARTIFACTS = bootstrap.ARTIFACTS
GITHUB = os.environ['OPENFLOWS_E2E_GITHUB_URL']
GITHUB_CONTAINER = os.environ['OPENFLOWS_E2E_GITHUB_CONTAINER']
CHATS = {}
WORKSPACES = {}
HOOK_DENIALS = []


def github(path, body=None, token='ci-operator-token'):
    return bootstrap.request(GITHUB, '/repos/test/repo' + path, body, token)


def docker(*args):
    return subprocess.run(['docker', 'exec', GITHUB_CONTAINER, *args],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, check=True, timeout=120).stdout.strip()


def save(filename, data):
    (ARTIFACTS / filename).write_text(json.dumps(data, indent=2))


def observe():
    workspaces = bootstrap.coder('/workspaces')['workspaces']
    for workspace in workspaces:
        WORKSPACES[workspace['id']] = workspace
        for resource in workspace['latest_build']['resources']:
            for agent in resource.get('agents') or []:
                logs = bootstrap.coder('/workspaceagents/' + agent['id'] + '/logs?after=0')
                save('agent-' + agent['id'] + '.json', logs)
                if agent.get('lifecycle_state') in ('start_error', 'start_timeout'):
                    raise RuntimeError(f'Production workspace startup failed: {workspace["name"]}; see agent logs')
    for chat in bootstrap.coder('/chats'):
        CHATS[chat['id']] = chat
        messages = bootstrap.coder('/chats/' + chat['id'] + '/messages?limit=100')
        save('chat-' + chat['id'] + '.json', {'state': chat, 'messages': messages})
        errors = []
        for message in messages['messages']:
            for part in message.get('content', []):
                if part['type'] != 'tool-result':
                    continue
                raw_text = json.dumps(part)
                if 'source modifications require an approved plan' in raw_text:
                    if part not in HOOK_DENIALS:
                        HOOK_DENIALS.append(part)
                    continue
                result = part.get('result') or {}
                if isinstance(result, str):
                    try:
                        result = json.loads(result)
                    except ValueError:
                        result = {}
                failed = isinstance(result, dict) and (result.get('success') is False or
                    result.get('exit_code', 0) not in (0, None))
                if part.get('is_error') or failed:
                    errors.append(part)
        if errors:
            raise RuntimeError(f'Controller-created chat had a failed tool call: {errors[0]}')
        if chat['status'] == 'error':
            raise RuntimeError(f'Controller-created chat failed: {chat}')
    save('workspaces.json', list(WORKSPACES.values()))
    return workspaces


def operator_status():
    result = subprocess.run(['openflows', 'status', '--tenant', bootstrap.TENANT, '--json'],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30, check=True)
    data = json.loads(result.stdout)
    save('operator-status.json', data)
    return data[0]


def lifecycle(nexus):
    command = 'env OPENFLOWS_TICKET=T-001 OPENFLOWS_ROLE=forge /opt/openflows-dev/openflows-harness status get'
    state = json.loads(bootstrap.ssh(nexus, command))
    save('lifecycle.json', state)
    return state


def ci(sha):
    docker('python3', '/opt/fixtures/run_ci.py', '--api', 'http://localhost:8080',
        '--remote', 'git://localhost:9418/repo.git', '--sha', sha,
        '--oracle', '/opt/fixtures/acceptance.sh', '--artifacts', '/tmp/github/ci-artifacts')


def journal():
    return [json.loads(line) for line in docker('cat', '/tmp/github/github-requests.jsonl').splitlines()]


def assert_no_merge_attempt():
    assert not any(entry['method'] == 'PUT' and entry['path'].endswith('/merge') for entry in journal()), \
        'The controller attempted a merge before all gates passed'


def collect_verifications():
    verification = []
    decoder = json.JSONDecoder()
    for chat in CHATS.values():
        messages = bootstrap.coder('/chats/' + chat['id'] + '/messages?limit=100')
        for message in messages['messages']:
            for part in message.get('content', []):
                if part['type'] != 'tool-result' or part.get('tool_name') != 'execute':
                    continue
                result = part.get('result') or {}
                if isinstance(result, str):
                    result = json.loads(result)
                output = result.get('output') or ''
                idx = 0
                while idx < len(output):
                    brace = output.find('{', idx)
                    if brace == -1:
                        break
                    try:
                        evidence, end = decoder.raw_decode(output, brace)
                    except ValueError:
                        idx = brace + 1
                        continue
                    idx = end
                    if isinstance(evidence, dict) and 'head_sha' in evidence and 'executor' in evidence:
                        verification.append(evidence)
    return verification


def wait_for_reworked_pr_review(nexus, pr_number, previous_head, label):
    def reworked():
        observe()
        assert_no_merge_attempt()
        current_pr = github('/pulls/' + str(pr_number))
        assert not current_pr['merged'], f'PR merged prematurely during {label}'
        current_head = current_pr['head']['sha']
        if current_head == previous_head:
            return False
        state = lifecycle(nexus)
        decision = state.get('pr_decision') or {}
        if (state.get('phase') == 'submit' and
                state.get('head') == current_head and
                state.get('verified_head') == current_head and
                decision.get('approved') and
                decision.get('head') == current_head and
                bool(state.get('pr_delivery'))):
            return current_head, state
        return False
    return bootstrap.wait(f'FORGE/SENTINEL did not complete rework and PR review for {label}',
        reworked, seconds=360)


def verify_security_boundaries(nexus, pair_id='T-001'):
    probe = f"""python3 - <<'PY'
import hashlib, json, os, urllib.error, urllib.request

def post(url, body, headers=None):
    req_headers = {{'Content-Type': 'application/json'}}
    if headers:
        req_headers.update(headers)
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=req_headers, method='POST')
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, None

hook_body = {{'type': 'pre_tool_use', 'meta': {{'dispatch_id': 'd1', 'chat_id': 'c1'}}, 'data': {{}}}}
code1, _ = post('http://localhost:3001/experimental/hooks/chat', hook_body)
code2, _ = post('http://localhost:3001/experimental/hooks/chat', hook_body, {{'Authorization': 'Bearer forged.invalid.jwt'}})

secret = os.environ['CODER_CHAT_HOOK_SECRET']
valid_token = hashlib.sha256(b'openflows-a2a-pair-token-v1\\0' + secret.encode() + b'\\0{pair_id}').hexdigest()

claim_params = {{'pair_id': '{pair_id}', 'role': 'forge', 'checkout_version': 3}}
_, forged_rpc = post('http://localhost:3000/rpc', {{
    'jsonrpc': '2.0',
    'id': 1,
    'method': 'tasks/claim',
    'params': {{**claim_params, 'pair_token': 'forged'}},
}})
_, valid_rpc = post('http://localhost:3000/rpc', {{
    'jsonrpc': '2.0',
    'id': 2,
    'method': 'tasks/claim',
    'params': {{**claim_params, 'pair_token': valid_token}},
}})
print(json.dumps({{'code1': code1, 'code2': code2, 'forged_rpc': forged_rpc, 'valid_rpc': valid_rpc}}))
PY"""
    result = json.loads(bootstrap.ssh(nexus, probe).strip().splitlines()[-1])
    assert result['code1'] == 401 and result['code2'] == 401, \
        f'Hook server did not reject unsigned/forged JWTs: {result}'
    forged_rpc = result['forged_rpc']
    assert 'error' in forged_rpc, f'A2A relay did not reject forged pair_token: {forged_rpc}'
    valid_rpc = result['valid_rpc']
    assert 'error' not in valid_rpc and 'result' in valid_rpc and valid_rpc['result'] is None, \
        f'A2A relay did not accept valid pair_token for registered pair {pair_id}: {valid_rpc}'


def restart_controller(nexus, expected_head):
    restart_cmd = (
        "set -eu; "
        "pid=$(pgrep -f 'openflows run' | head -n1); "
        "cp /proc/$pid/environ /tmp/openflows-controller.env0; "
        "kill $pid; "
        "for i in $(seq 1 10); do "
        "  if ! kill -0 $pid 2>/dev/null; then break; fi; "
        "  sleep 1; "
        "done; "
        "cd /home/coder/workspace; "
        "python3 -c 'import os, subprocess; "
        "env = dict(item.split(\"=\", 1) for item in open(\"/tmp/openflows-controller.env0\", \"rb\").read().decode().split(\"\\0\") if \"=\" in item); "
        "log = open(\"/tmp/openflows-controller.log\", \"ab\"); "
        "subprocess.Popen([\"openflows\", \"run\"], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)'; "
        "for i in $(seq 1 30); do "
        "  if curl -fsS http://localhost:3001/experimental/hooks/health >/dev/null 2>&1; then exit 0; fi; "
        "  sleep 1; "
        "done; "
        "exit 1"
    )
    bootstrap.ssh(nexus, restart_cmd)
    state = lifecycle(nexus)
    assert state['phase'] == 'submit' and state['head'] == expected_head
    assert state['revision'] == 2 and state['pr_decision']['approved']
    assert not state.get('pr_human')
    assert_no_merge_attempt()


def run():
    workspace, templates, organization, nexus = bootstrap.setup_tenant('system-e2e')
    try:
        original_main = docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', 'refs/heads/main')
        issue = github('/issues', {'title': 'Fix answer.txt to return 42',
            'body': 'The committed answer is 41. Change it to 42, preserve the CI workflow, '
                'and verify the exact committed candidate through SENTINEL before opening a PR.'})
        assert issue['number'] == 1
        save('issue.json', issue)
        print('Created the issue through GitHub; waiting for plan rejection, A2A failure/repair, and PR.', flush=True)

        def pull_opened():
            observe()
            pulls = github('/pulls')
            return pulls[0] if pulls else False
        pr = bootstrap.wait('NEXUS/FORGE/SENTINEL did not reach an actual PR', pull_opened, seconds=600)
        assert len(github('/pulls')) == 1
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', 'refs/heads/main') == original_main, \
            'Agent work changed the default branch before the merge gates passed'
        save('candidate-pr.json', pr)
        head_1 = pr['head']['sha']
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'show', head_1 + ':answer.txt') == '42'
        assert len(HOOK_DENIALS) >= 1, 'Expected live PreToolUse hook denial before plan approval'

        def reviewed_initial():
            observe()
            state = lifecycle(nexus)
            decision = state.get('pr_decision') or {}
            return state if (state.get('phase') == 'submit' and
                state.get('head') == head_1 and
                decision.get('approved') and
                decision.get('head') == head_1 and
                bool(state.get('pr_delivery'))) else False
        state = bootstrap.wait('SENTINEL did not approve its initial PR review', reviewed_initial, seconds=240)
        verify_security_boundaries(nexus, 'T-001')
        assert state['revision'] == 2, f'Expected plan revision 2 after rejection/revision loop, got {state["revision"]}'
        assert state['head'] == head_1 and state['verified_head'] == head_1
        assert state['plan_decision']['approved'] and state['test_decision']['approved']
        assert not state.get('pr_human')
        history_phases = [(h['from'], h['to']) for h in state.get('history', [])]
        assert ('plan_ready', 'plan_rejected') in history_phases, f'Missing plan rejection in history: {history_phases}'
        assert ('testing', 'building') in history_phases, f'Missing testing rejection in history: {history_phases}'

        verifications_initial = collect_verifications()
        assert any(v['exit_code'] == 1 and v['head_sha'] != head_1 for v in verifications_initial), \
            f'Expected failed A2A verification on broken candidate 40: {verifications_initial}'
        assert any(v['exit_code'] == 0 and v['head_sha'] == head_1 and 'ACCEPTANCE_OK' in v['stdout']
            for v in verifications_initial), f'Expected passing A2A verification on {head_1}: {verifications_initial}'
        print('Verified PreToolUse denial, plan rejection loop, A2A failure/rejection loop, and initial PR review.', flush=True)

        # ── Rework Loop 1: Failing CI check-run triggers autonomous /ci_fix ──
        github('/check-runs', {
            'name': 'acceptance',
            'head_sha': head_1,
            'status': 'completed',
            'conclusion': 'failure',
        }, 'ci-runner-token')
        head_2, state = wait_for_reworked_pr_review(nexus, pr['number'], head_1, 'CI failure /ci_fix')
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'show', head_2 + ':answer.txt') == '42'
        print(f'CI failure on {head_1[:8]} triggered /ci_fix; FORGE pushed {head_2[:8]} and passed A2A + Sentinel review.', flush=True)

        # ── Rework Loop 2: Human requests changes (Round 1) ──
        ci(head_2)
        github('/pulls/' + str(pr['number']) + '/reviews', {
            'event': 'REQUEST_CHANGES',
            'commit_id': head_2,
            'body': 'REQUEST_CHANGES_ROUND_1: please address round 1 human review feedback',
        }, 'ci-reviewer-token')
        head_3, state = wait_for_reworked_pr_review(nexus, pr['number'], head_2, 'human REQUEST_CHANGES round 1')
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'show', head_3 + ':answer.txt') == '42'
        print(f'Human REQUEST_CHANGES round 1 on {head_2[:8]} triggered /address_review; FORGE pushed {head_3[:8]}.', flush=True)

        # ── Rework Loop 3: Human requests changes AGAIN (Round 2) ──
        ci(head_3)
        github('/pulls/' + str(pr['number']) + '/reviews', {
            'event': 'REQUEST_CHANGES',
            'commit_id': head_3,
            'body': 'REQUEST_CHANGES_ROUND_2: please address round 2 human review feedback',
        }, 'ci-reviewer-token')
        head_4, state = wait_for_reworked_pr_review(nexus, pr['number'], head_3, 'human REQUEST_CHANGES round 2')
        head = head_4
        assert len({head_1, head_2, head_3, head_4}) == 4
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'show', head + ':answer.txt') == '42'
        print(f'Human REQUEST_CHANGES round 2 on {head_3[:8]} triggered /address_review again; FORGE pushed {head[:8]}.', flush=True)

        for ws in bootstrap.coder('/workspaces')['workspaces']:
            if ws['id'] == workspace['id']:
                continue
            name = 'ci/' + ws['name']
            if 'forge' in ws['name']:
                (ARTIFACTS / 'forge-verify.log').write_text(bootstrap.ssh(name,
                    'cat /home/coder/.local/state/openflows/verify.log'))
        verification = collect_verifications()
        passing_final = [v for v in verification if v['head_sha'] == head and v['exit_code'] == 0]
        assert passing_final, f'Missing passing A2A verification for final head {head}'
        evidence = passing_final[-1]
        forge = next(ws for ws in WORKSPACES.values() if 'forge' in ws['name'])
        assert evidence['executor']['workspace'] == forge['id']
        assert 'ACCEPTANCE_OK' in evidence['stdout']
        cwd = next(line.split('=', 1)[1] for line in evidence['stdout'].splitlines() if line.startswith('CHECKOUT_CWD='))
        assert not cwd.startswith('/home/coder/workspace')
        assert evidence['task_id'] == state['verification_task']
        save('verification.json', evidence)

        ci(head)
        hold_start = len(bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log').splitlines())
        def held_for_human():
            assert not github('/pulls/' + str(pr['number']))['merged'], 'Merged without human approval'
            observe()
            assert_no_merge_attempt()
            log = re.sub(r'\x1b\[[0-9;]*m', '', bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log'))
            return any('Deferring PR: waiting for lifecycle approvals or review delivery' in line and
                'human_approved=false' in line and 'review_approved=true' in line
                for line in log.splitlines()[hold_start:])
        bootstrap.wait('VESSEL did not hold the reviewed PR for human approval', held_for_human, seconds=90)
        save('before-human.json', lifecycle(nexus))
        assert not lifecycle(nexus).get('pr_human')
        assert_no_merge_attempt()

        # ── Controller restart & reconciliation mid-journey ──
        restart_controller(nexus, head)
        print('Controller restarted and reconciled durable Redis/Coder/GitHub state without losing progress.', flush=True)

        # A pending check is a declared external state, never a fabricated success.
        github('/check-runs', {'name': 'acceptance', 'head_sha': head, 'status': 'in_progress'}, 'ci-runner-token')

        # ── #396: a second reviewer approves after rework addressed the operator's change request ──
        # The operator's latest GitHub review is still the round 2 REQUEST_CHANGES on head_3, which
        # the rework to head_4 addressed. It must not hide another reviewer's approval of head_4.
        github('/pulls/' + str(pr['number']) + '/reviews', {'event': 'APPROVE', 'commit_id': head,
            'body': 'Second reviewer approves the reworked candidate'}, 'ci-reviewer-token')
        def reconciled_second_reviewer():
            assert not github('/pulls/' + str(pr['number']))['merged'], 'Merged while CI was pending'
            assert_no_merge_attempt()
            human = lifecycle(nexus).get('pr_human') or {}
            return bool(human.get('approved')) and human.get('head') == head and \
                'Approved on GitHub by reviewer' in human.get('report', '')
        bootstrap.wait('VESSEL did not reconcile the second reviewer approval past the addressed change request',
            reconciled_second_reviewer, seconds=120)
        save('after-reviewer-approval.json', lifecycle(nexus))
        print('Second reviewer approval of the reworked head reconciled past the addressed change request.', flush=True)
        state = lifecycle(nexus)  # Reconciliation may have completed review delivery.

        subprocess.run(['openflows', 'gate', 'decide', '--tenant', bootstrap.TENANT, '--ticket', 'T-001',
            '--phase', 'submit', '--revision', str(state['revision']), '--round', str(state['review_round']),
            '--head', head, '--verdict', 'approve', '--notes', 'Operator approves the verified candidate'],
            stdin=subprocess.DEVNULL, check=True, timeout=30)
        github('/pulls/' + str(pr['number']) + '/reviews',
            {'event': 'APPROVE', 'commit_id': head, 'body': 'Human approval of the verified candidate'},
            'ci-reviewer-token')
        bootstrap.wait('Approved SENTINEL review delivery did not complete',
            lambda: not lifecycle(nexus).get('pr_delivery'), seconds=90)
        pending_start = len(journal())
        pending_log_start = len(bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log').splitlines())
        def polled_pending_ci():
            assert not github('/pulls/' + str(pr['number']))['merged'], 'Merged while CI was pending'
            assert_no_merge_attempt()
            polled = any(entry['method'] == 'GET' and entry['path'] == f'/repos/test/repo/commits/{head}/check-suites' and
                any(check['status'] == 'in_progress' for check in entry['response'].get('check_suites', []))
                for entry in journal()[pending_start:])
            log = re.sub(r'\x1b\[[0-9;]*m', '', bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log'))
            waiting = any('Waiting for CI before merge' in line and f'head={head}' in line and
                f"pr={pr['number']}" in line for line in log.splitlines()[pending_log_start:])
            assert_no_merge_attempt()
            return polled and waiting
        bootstrap.wait('VESSEL did not wait for pending CI after review delivery', polled_pending_ci, seconds=90)
        print('Human approval and pending CI each prevented premature merging; running trusted acceptance.', flush=True)
        ci(head)
        merged_pr = bootstrap.wait('Actual VESSEL did not merge the approved and tested candidate',
            lambda: (current if (current := github('/pulls/' + str(pr['number'])))['merged'] else False))
        merged = merged_pr['merge_commit_sha']
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', 'main') == merged
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', merged + '^') == pr['base']['sha']
        assert docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', merged + '^{tree}') == \
            docker('git', '--git-dir=/tmp/github/repo.git', 'rev-parse', head + '^{tree}')
        ci(merged)

        def cleaned():
            remaining = observe()
            status = operator_status()
            ticket = next(t for t in status['tickets'] if t['id'] == 'T-001')
            return len(remaining) == 1 and remaining[0]['id'] == workspace['id'] and \
                ticket['status']['type'] == 'merged'
        bootstrap.wait('Controller did not complete the ticket and remove both role workspaces', cleaned, seconds=240)
        assert github('/issues/1')['state'] == 'closed'
        log = bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log')
        (ARTIFACTS / 'controller.log').write_text(log)
        log = re.sub(r'\x1b\[[0-9;]*m', '', log)
        for chat_id in CHATS:
            chat_log = '\n'.join(line for line in log.splitlines() if 'chat_id=' + chat_id in line)
            for event in ('SessionStart', 'PreToolUse', 'PostToolUse', 'Stop'):
                assert 'event=' + event in chat_log, f'No authenticated {event} hook for chat {chat_id}'
        save('system-evidence.json', {'full_issue_to_merge': True, 'issue': issue, 'pr': merged_pr,
            'candidate_heads': [head_1, head_2, head_3, head_4], 'tested_head': head, 'merged_head': merged,
            'hook_denials': len(HOOK_DENIALS), 'role_chats': list(CHATS.values()),
            'workspaces': list(WORKSPACES.values()), 'verification': state['verification_task']})
        bootstrap.coder('/workspaces/' + workspace['id'] + '/builds', {'transition': 'delete'})
        bootstrap.wait('NEXUS deletion did not complete', lambda: not bootstrap.coder('/workspaces')['workspaces'])
        print('PASS: actual issue routing, role chats, signed hooks, PreToolUse denial, plan/testing rejection loops, CI failure /ci_fix rework, repeated human REQUEST_CHANGES rework, second-reviewer approval after addressed change requests, controller restart recovery, human/CI gates, squash merge, acceptance and cleanup.', flush=True)
    finally:
        errors = []
        for collect in (observe, lambda: (ARTIFACTS / 'controller-final.log').write_text(
                bootstrap.ssh(nexus, 'cat /tmp/openflows-controller.log'))):
            try:
                collect()
            except Exception as error:
                errors.append(str(error))
        if errors:
            save('final-diagnostics-errors.json', errors)


if __name__ == '__main__':
    run()
