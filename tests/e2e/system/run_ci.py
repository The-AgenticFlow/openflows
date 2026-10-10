"""Independently execute the trusted oracle against an exact Git commit."""
import argparse
import json
import os
import subprocess
import tempfile
import urllib.request
from pathlib import Path

from git_repository import git


def run(api, remote, sha, oracle, artifacts):
    if len(sha) != 40 or any(c not in '0123456789abcdef' for c in sha):
        raise ValueError('CI requires an exact SHA')
    oracle = oracle.resolve(strict=True)
    artifacts.mkdir(parents=True, exist_ok=True)

    def publish(status, conclusion=None, output=None):
        body = {'name': 'acceptance', 'head_sha': sha, 'status': status,
            'conclusion': conclusion, 'output': output}
        request = urllib.request.Request(api + '/repos/test/repo/check-runs',
            json.dumps(body).encode(), {'Authorization': 'Bearer ci-runner-token',
            'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=10) as response:
            if response.status != 201:
                raise RuntimeError('CI publication failed')

    publish('in_progress')
    with tempfile.TemporaryDirectory(prefix='openflows-acceptance-') as directory:
        checkout = Path(directory) / 'candidate'
        git('clone', '--no-checkout', remote, str(checkout))
        git('checkout', '--detach', sha, cwd=checkout)
        if git('rev-parse', 'HEAD', cwd=checkout) != sha:
            raise RuntimeError('CI checkout does not match requested head')
        try:
            result = subprocess.run(['sh', str(oracle)], cwd=checkout, text=True,
                capture_output=True, timeout=20,
                env={'PATH': os.defpath, 'HOME': directory, 'LANG': 'C'})
            code, stdout, stderr = result.returncode, result.stdout, result.stderr
            conclusion = 'success' if code == 0 else 'failure'
        except subprocess.TimeoutExpired:
            code, stdout, stderr, conclusion = 124, '', 'Acceptance timed out', 'timed_out'
        evidence = {'head_sha': sha, 'checkout': str(checkout), 'exit_code': code,
            'stdout': stdout, 'stderr': stderr, 'oracle': str(oracle)}
        (artifacts / f'{sha}.json').write_text(json.dumps(evidence, indent=2))
        publish('completed', conclusion, {'title': 'Independent acceptance',
            'summary': f'exit_code={code}', 'text': stdout + stderr})
        return 0 if code == 0 else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--api', required=True)
    parser.add_argument('--remote', required=True)
    parser.add_argument('--sha', required=True)
    parser.add_argument('--oracle', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(run(args.api, args.remote, args.sha, args.oracle, args.artifacts))
