"""Small stateful GitHub API adapter backed by real Git, never blanket success."""
import subprocess
from datetime import datetime, timezone
from urllib.parse import parse_qs, urlsplit

from git_repository import Repository

TOKENS = {'ci-forge-token': 'forge', 'ci-sentinel-token': 'sentinel',
    'ci-vessel-token': 'vessel', 'ci-runner-token': 'ci', 'ci-operator-token': 'operator'}


class GitHub:
    def __init__(self, root, reject, git_host='127.0.0.1', git_port=0, remote_host='127.0.0.1'):
        self.repo = Repository(root, git_host, git_port, remote_host)
        self.reject = reject
        self.issues = {}
        self.pulls = {}
        self.reviews = {}
        self.comments = {}
        self.checks = {}
        self.next_number = 1

    def fail(self, code, message):
        raise self.reject(code, message)

    def require_role(self, role, allowed):
        if role not in allowed:
            self.fail(403, 'Disposable credential cannot perform this operation')

    def pull(self, number):
        if number not in self.pulls:
            self.fail(404, 'PR not found')
        pr = self.pulls[number]
        # A branch push updates the PR head without a fixture-side injection.
        pr['head']['sha'] = self.repo.head(pr['head']['ref'])
        pr['base']['sha'] = self.repo.head(pr['base']['ref'])
        return pr

    def page(self, values, query):
        page = max(1, int(query.get('page', ['1'])[0]))
        size = min(100, max(1, int(query.get('per_page', ['100'])[0])))
        return values[(page - 1) * size:page * size]

    def handle(self, method, url, body, role):
        parsed = urlsplit(url)
        query = parse_qs(parsed.query)
        parts = parsed.path.strip('/').split('/')
        if method == 'GET' and parts == ['user']:
            return 200, {'login': role, 'id': list(TOKENS.values()).index(role) + 1}
        if parts[:3] != ['repos', 'test', 'repo']:
            self.fail(404, 'Unsupported fixture repository or endpoint')
        route = parts[3:]
        if method == 'GET' and not route:
            return 200, {'full_name': 'test/repo', 'default_branch': 'main',
                'clone_url': self.repo.url, 'name': 'repo', 'owner': {'login': 'test'}}
        if route == ['issues']:
            if method == 'POST':
                self.require_role(role, ['operator'])
                number = self.next_number
                self.next_number += 1
                issue = {'number': number, 'title': body['title'], 'body': body.get('body', ''),
                    'html_url': f'http://github-fixture/test/repo/issues/{number}',
                    'state': 'open', 'labels': [], 'assignees': []}
                self.issues[number] = issue
                return 201, issue
            if method == 'GET':
                state = query.get('state', ['open'])[0]
                return 200, self.page([i for i in self.issues.values()
                    if state == 'all' or i['state'] == state], query)
        if len(route) >= 2 and route[0] == 'issues':
            number = int(route[1])
            issue = self.issues.get(number) or self.pulls.get(number)
            if not issue:
                self.fail(404, 'Issue not found')
            if len(route) == 2:
                if method == 'GET':
                    return 200, issue
                if method == 'PATCH':
                    for field in ('state', 'assignees', 'labels'):
                        if field in body:
                            if field == 'state' and body[field] not in ('open', 'closed'):
                                self.fail(422, 'Invalid issue state')
                            issue[field] = body[field]
                    return 200, issue
            if route[2:] == ['comments']:
                comments = self.comments.setdefault(number, [])
                if method == 'POST':
                    comment = {'id': len(comments) + 1, 'body': body['body'], 'user': {'login': role}}
                    comments.append(comment)
                    return 201, comment
                if method == 'GET':
                    return 200, self.page(comments, query)
        if route == ['pulls']:
            if method == 'POST':
                self.require_role(role, ['forge'])
                head, base = body['head'], body['base']
                if head == base:
                    self.fail(422, 'Head and base must differ')
                number = self.next_number
                self.next_number += 1
                pr = {'number': number, 'title': body['title'], 'body': body.get('body', ''),
                    'state': 'open', 'merged': False, 'mergeable': None, 'merge_commit_sha': None,
                    'html_url': f'http://github-fixture/test/repo/pull/{number}',
                    'head': {'ref': head, 'sha': self.repo.head(head)},
                    'base': {'ref': base, 'sha': self.repo.head(base)}}
                self.pulls[number] = pr
                return 201, pr
            if method == 'GET':
                state = query.get('state', ['open'])[0]
                return 200, self.page([self.pull(n) for n, pr in self.pulls.items()
                    if state == 'all' or pr['state'] == state], query)
        if len(route) >= 2 and route[0] == 'pulls':
            number = int(route[1])
            pr = self.pull(number)
            if len(route) == 2 and method == 'GET':
                return 200, pr
            if route[2:] == ['reviews']:
                reviews = self.reviews.setdefault(number, [])
                if method == 'GET':
                    return 200, self.page(reviews, query)
                if method == 'POST':
                    self.require_role(role, ['sentinel', 'operator'])
                    states = {'APPROVE': 'APPROVED', 'REQUEST_CHANGES': 'CHANGES_REQUESTED', 'COMMENT': 'COMMENTED'}
                    if body.get('event') not in states or body.get('comments'):
                        self.fail(422, 'Unsupported review event or inline comments')
                    sha = body.get('commit_id') or pr['head']['sha']
                    self.repo.require_commit(sha)
                    review = {'id': len(reviews) + 1, 'state': states[body['event']],
                        'body': body.get('body', ''), 'user': {'login': role}, 'commit_id': sha,
                        'author_association': 'COLLABORATOR',
                        'submitted_at': datetime.now(timezone.utc).isoformat()}
                    reviews.append(review)
                    return 201, review
            if route[2:] == ['merge'] and method == 'PUT':
                self.require_role(role, ['vessel'])
                if pr['merged'] or pr['state'] != 'open':
                    self.fail(405, 'PR is not open')
                sha = pr['head']['sha']
                if body.get('sha') != sha:
                    self.fail(409, 'PR head changed')
                if body.get('merge_method') != 'merge':
                    self.fail(422, 'Only merge commits are supported by this scenario')
                check = self.checks.get(sha)
                if not check or check['status'] != 'completed' or check['conclusion'] != 'success':
                    self.fail(405, 'Exact-head acceptance check is not successful')
                relevant = [r for r in self.reviews.get(number, [])
                    if r['user']['login'] == 'sentinel' and r['state'] != 'COMMENTED']
                if not relevant or relevant[-1]['state'] != 'APPROVED' or relevant[-1]['commit_id'] != sha:
                    self.fail(405, 'Current head lacks SENTINEL approval')
                merged = self.repo.merge(pr['base']['ref'], pr['head']['ref'], sha, pr['base']['sha'],
                    body.get('commit_title') or f'Merge PR #{number}')
                pr.update(state='closed', merged=True, merge_commit_sha=merged)
                return 200, {'merged': True, 'sha': merged, 'message': 'Real Git merge completed'}
        if route == ['check-runs'] and method == 'POST':
            self.require_role(role, ['ci'])
            sha = body['head_sha']
            self.repo.require_commit(sha)
            if body.get('name') != 'acceptance' or body.get('status') not in ('in_progress', 'completed'):
                self.fail(422, 'Unsupported check')
            if body['status'] == 'completed' and body.get('conclusion') not in ('success', 'failure', 'timed_out'):
                self.fail(422, 'Unsupported check conclusion')
            check = dict(body, id=1, app={'slug': 'github-actions', 'name': 'GitHub Actions'})
            self.checks[sha] = check
            return 201, check
        if len(route) == 3 and route[0] == 'commits' and method == 'GET':
            sha, endpoint = route[1:]
            self.repo.require_commit(sha)
            check = self.checks.get(sha)
            if endpoint == 'status':
                return 200, {'state': 'pending', 'total_count': 0, 'statuses': []}
            if endpoint == 'check-runs':
                runs = self.page([check] if check else [], query)
                return 200, {'total_count': len(runs), 'check_runs': runs}
            if endpoint == 'check-suites':
                suites = [dict(check, latest_check_runs_count=1)] if check else []
                return 200, {'total_count': len(suites), 'check_suites': self.page(suites, query)}
        self.fail(404, 'Unsupported fixture request')
