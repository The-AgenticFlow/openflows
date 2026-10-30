# Connected-local GitHub App live test

Run this only with the team-owned GitHub App and a disposable repository that
the tester owns. This validates the real GitHub device grant and token rotation;
the automated tests use a mock GitHub endpoint. Record the App slug, test repo,
date, and pass/fail results, but never record access or refresh tokens.

## Prerequisites

- The App has **Device flow** enabled and is installed on the disposable repo
  with Contents read/write permission. The tester has write access to that repo.
- Set `OPENFLOWS_GITHUB_AUTH_MODE=connected-local`,
  `OPENFLOWS_GITHUB_CLIENT_ID` (the public client ID, not the App ID), and
  `OPENFLOWS_GITHUB_APP_SLUG` in the local `.env`. Do not add an App secret.
- Docker, Rust, and the local setup prerequisites in [Quick Start](../../quick_start.md)
  are available. Work from the repository root.

## 1. Approve the device and check repository visibility

```sh
./scripts/setup.sh owner/disposable-repo --fleet 1
cargo run -q -p openflows-manager -- --config .env github-repos
```

The first command displays a code and GitHub verification URL. Approve it as
the tester. The second command must list `owner/disposable-repo`. Check that a
repo the tester cannot access is absent. The setup must not ask for an App
private key, client secret, or a new GitHub App.

## 2. Exercise real refresh without waiting eight hours

This changes only the locally stored access-token expiry timestamp. It does
not replace either token. Close other local OpenFlows commands while doing
this. The script acquires the same lock used by the Manager, and writes the
file atomically with mode `0600`.

```sh
python3 - <<'PY'
import fcntl, json, os, pathlib, subprocess, tempfile
path = pathlib.Path.home() / '.config/openflows/github-user.json'
lock = path.with_suffix('.lock')
with lock.open('a+b') as guard:
    fcntl.flock(guard, fcntl.LOCK_EX)
    original = json.loads(path.read_text())
    expired = dict(original, access_expires_at=0)
    fd, temporary = tempfile.mkstemp(prefix='.github-live-test-', dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'w') as output:
            json.dump(expired, output)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
subprocess.run(['cargo', 'run', '-q', '-p', 'openflows-manager', '--',
                '--config', '.env', 'github-repos'], check=True)
rotated = json.loads(path.read_text())
assert rotated['access_token'] != original['access_token']
assert rotated['refresh_token'] != original['refresh_token']
print('Both GitHub credentials rotated')
PY
```

The repository list must still work and the script must report that **both**
tokens rotated. It never prints token values. Restart the local Manager and run
`github-repos` again. It must continue to work without another device approval.

## 3. Verify a push from a local runtime

In a running OpenFlows runtime workspace for the disposable repo, create a
throwaway branch and push an empty commit from its checkout:

```sh
git switch -c openflows-live-auth-test
git commit --allow-empty -m 'test: connected-local GitHub authorization'
git push -u origin openflows-live-auth-test
```

Confirm on GitHub that the push was performed with the signed-in user's access
and the branch appeared only in the disposable repo. Commit authorship depends
on the workspace Git configuration. Delete the test branch on GitHub
afterward. If the workspace has no checkout yet, let OpenFlows start a test
issue first and use the checkout it creates. Do not run these commands in the
OpenFlows source repository.

## 4. Verify sign-out and reauthorization

```sh
cargo run -q -p openflows-manager -- --config .env github-logout
cargo run -q -p openflows-manager -- --config .env github-repos
```

The second command must fail with `GITHUB_REAUTH_REQUIRED`. Re-run
`github-login`, approve the new device code, and confirm `github-repos` works
again. Stop the disposable local workspaces when finished.

## Evidence to record

- App slug and disposable repo name; device approval succeeded.
- Repository visibility matched the tester's GitHub access.
- Forced refresh rotated both tokens; restart reused the new pair.
- Runtime push succeeded as the tester, or the exact failure without secrets.
- Sign-out blocked access; reauthorization restored it.
- Hosted SaaS regression checks passed separately.

Never attach `.env`, `github-user.json`, broker keys, token hashes, or command
output containing credential values.
