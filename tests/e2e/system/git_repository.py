"""Real disposable repository and Git transport for the system fixture."""
import os
import socket
import shutil
import subprocess
import time
from pathlib import Path


def git(*args, cwd=None, input=None):
    env = dict(os.environ, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
        GIT_TERMINAL_PROMPT='0', GIT_AUTHOR_NAME='E2E', GIT_AUTHOR_EMAIL='e2e@example.test',
        GIT_COMMITTER_NAME='E2E', GIT_COMMITTER_EMAIL='e2e@example.test')
    return subprocess.run(['git', *args], cwd=cwd, env=env, text=True,
        capture_output=True, check=True, timeout=20, input=input).stdout.strip()


class Repository:
    def __init__(self, root, host='127.0.0.1', port=0, remote_host='127.0.0.1'):
        self.root = root
        self.bare = root / 'repo.git'
        if self.bare.exists():
            raise RuntimeError('Fixture requires a fresh disposable directory')
        git('init', '--bare', '--initial-branch=main', str(self.bare))
        # Match the owner/repo clone URL used by the production bootstrap.
        (root / 'test').mkdir()
        (root / 'test' / 'repo.git').symlink_to('../repo.git')
        seed = root / 'seed'
        git('clone', str(self.bare), str(seed))
        (seed / 'answer.txt').write_text('41\n')
        workflows = seed / '.github' / 'workflows'
        workflows.mkdir(parents=True)
        shutil.copyfile(Path(__file__).with_name('bootstrap-ci.yml'), workflows / 'ci.yml')
        git('add', 'answer.txt', '.github/workflows/ci.yml', cwd=seed)
        git('commit', '-m', 'seed deliberately broken implementation', cwd=seed)
        git('push', 'origin', 'main', cwd=seed)
        if not port:
            with socket.socket() as sock:
                sock.bind((host, 0))
                port = sock.getsockname()[1]
        self.url = f'git://{remote_host}:{port}/repo.git'
        self.log = (root / 'git-daemon.log').open('w')
        self.daemon = subprocess.Popen(['git', 'daemon', '--reuseaddr', '--export-all',
            '--enable=receive-pack', f'--base-path={root}', f'--listen={host}',
            f'--port={port}', str(root)], stdout=self.log, stderr=self.log)
        try:
            for _ in range(100):
                if self.daemon.poll() is not None:
                    raise RuntimeError('Git daemon exited before readiness')
                try:
                    with socket.create_connection(('127.0.0.1' if host == '0.0.0.0' else host, port), timeout=0.2):
                        break
                except OSError:
                    time.sleep(0.05)
            else:
                raise RuntimeError('Git daemon readiness timed out')
        except Exception:
            self.close()
            raise

    def close(self):
        self.daemon.terminate()
        try:
            self.daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.daemon.kill()
            self.daemon.wait(timeout=5)
        self.log.close()

    def head(self, branch):
        # Verify branch refs explicitly; a caller cannot pass Git options or revisions.
        git('check-ref-format', f'refs/heads/{branch}')
        return git('--git-dir', str(self.bare), 'rev-parse', '--verify', f'refs/heads/{branch}')

    def require_commit(self, sha):
        if len(sha) != 40 or any(c not in '0123456789abcdef' for c in sha):
            raise ValueError('Expected an exact commit SHA')
        git('--git-dir', str(self.bare), 'cat-file', '-e', f'{sha}^{{commit}}')

    def merge(self, base, head_branch, candidate, expected_base, title):
        worktree = self.root / 'merge-checkout'
        git('--git-dir', str(self.bare), 'worktree', 'add', '--detach', str(worktree), expected_base)
        try:
            git('merge', '--no-ff', '-m', title, candidate, cwd=worktree)
            merged = git('rev-parse', 'HEAD', cwd=worktree)
            # One ref transaction verifies the head and compares the base before
            # updating it. A push during the merge cannot authorize a stale head.
            transaction = (f'start\nverify refs/heads/{head_branch} {candidate}\n'
                f'update refs/heads/{base} {merged} {expected_base}\nprepare\ncommit\n')
            git('--git-dir', str(self.bare), 'update-ref', '--stdin', input=transaction)
            return merged
        finally:
            git('--git-dir', str(self.bare), 'worktree', 'remove', '--force', str(worktree))
