"""Upgrade, restart and roll back a running OpenFlows stack with setup's own stages (#528).

Two candidate releases are served from a local folder through OPENFLOWS_RELEASE_BASE_URL:
9.0.0 and 9.1.0. They hold the same programs, but 9.1.0's carry a marker so the test can
tell which ones a workspace runs, and 9.1.0 uses a re-tagged workspace image so its
templates differ, as a release that changes templates does. Setup runs from a copy of the
setup kit (the files release-assets.yml packs), not from this checkout.

1. Install 9.0.0, create a tenant, record Redis state.
2. Restart Redis, PostgreSQL and Coder; state, workspace and controller survive.
3. Upgrade to 9.1.0 without consent: programs and templates change, the running
   workspace keeps the old ones, and setup reports the upgrade incomplete (exit 3).
4. Re-run with --update-workspaces: the workspace runs 9.1.0, nothing is pending.
5. Upgrades that fail (missing release, bad checksum) leave 9.1.0 running.
6. Roll back to 9.0.0 with the workspace stopped: it moves to the 9.0.0 templates and
   stays stopped; started, it runs 9.0.0.
7. Restart again; state survives the round trip.
"""
import functools
import hashlib
import http.server
import json
import os
import re
import shutil
import subprocess
import tarfile
import threading
import uuid
from pathlib import Path

import production_bootstrap as pb

CHECKOUT = Path(os.environ['OPENFLOWS_E2E_CHECKOUT'])
SCRATCH = Path(os.environ['OPENFLOWS_E2E_SCRATCH'])
PROJECT = os.environ['OPENFLOWS_E2E_PROJECT']
ARTIFACTS = pb.ARTIFACTS
KIT = SCRATCH / 'kit'
BIN = KIT / '.dev-binaries'
RELEASES = SCRATCH / 'releases'
TARGET = 'x86_64-unknown-linux-gnu'
PROGRAMS = ('openflows', 'openflows-doctor', 'openflows-harness')
OLD, NEW = '9.0.0', '9.1.0'
IMAGE = {OLD: os.environ['TF_VAR_workspace_image'], NEW: os.environ['TF_VAR_workspace_image'] + '-upgrade'}
MARKER_KEY = 'openflows:e2e:upgrade-marker'
evidence = {'steps': []}


def step(text):
    print(text, flush=True)
    evidence['steps'].append(text)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def kit_paths():
    """The paths release-assets.yml packs into the setup kit, read from its tar command."""
    lines = (CHECKOUT / '.github/workflows/release-assets.yml').read_text().splitlines()
    start = next(i for i, line in enumerate(lines) if '-setup.tar.gz" --transform' in line)
    paths = []
    for line in lines[start + 1:]:
        paths += line.strip().rstrip('\\').split()
        if not line.rstrip().endswith('\\'):
            break
    return [p for p in paths if p != 'RELEASE_VERSION']


def make_kit():
    KIT.mkdir()
    for pattern in kit_paths():
        for source in sorted(CHECKOUT.glob(pattern)):
            target = KIT / source.relative_to(CHECKOUT)
            target.parent.mkdir(parents=True, exist_ok=True)
            if source.is_dir():
                shutil.copytree(source, target)
            else:
                shutil.copy2(source, target)
    (KIT / 'RELEASE_VERSION').write_text(OLD + '\n')
    (KIT / '.env').write_text('OPENFLOWS_SETUP_MODE=binary\n')


def publish(version, marker=None, tamper=False):
    """Write openflows-VERSION/openflows-VERSION-TARGET.tar.gz and its checksum."""
    name = f'openflows-{version}-{TARGET}'
    staging = SCRATCH / 'build' / name
    staging.mkdir(parents=True)
    for program in PROGRAMS:
        data = (SCRATCH / 'bin' / program).read_bytes()
        if marker:  # Trailing bytes change the file, not how it runs.
            data += f'\nopenflows-e2e-release {marker}\n'.encode()
        (staging / program).write_bytes(data)
        (staging / program).chmod(0o755)
    folder = RELEASES / f'openflows-{version}'
    folder.mkdir(parents=True)
    archive = folder / f'{name}.tar.gz'
    with tarfile.open(archive, 'w:gz') as tar:
        tar.add(staging, arcname=name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if tamper:
        with archive.open('ab') as handle:
            handle.write(b'tampered')
    (folder / f'{name}.tar.gz.sha256').write_text(f'{digest}  {name}.tar.gz\n')
    return {p: sha256(staging / p) for p in PROGRAMS}


def serve_releases():
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(RELEASES))
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f'http://127.0.0.1:{server.server_address[1]}'


def run_setup(version, stages, log_name, update_workspaces=False, check=True):
    """Run setup.sh stages from the kit, as `./scripts/setup.sh --binary` would for VERSION."""
    os.environ['TF_VAR_workspace_image'] = IMAGE.get(version, IMAGE[NEW])
    script = f'''
        set -uo pipefail
        source "{KIT}/scripts/setup.sh"
        MODE=binary REPO=test/repo UPDATE_WORKSPACES={'1' if update_workspaces else ''}
        CODER_TOKEN="$(env_get CODER_SESSION_TOKEN)"
        env_set OPENFLOWS_VERSION {version}
        {stages}
        printf '\\nUPGRADE_PENDING=%s\\n' "${{UPGRADE_PENDING:-}}"
    '''
    with (ARTIFACTS / log_name).open('w') as log:
        result = subprocess.run(['bash', '-c', script], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, timeout=1200)
        log.write(result.stdout)
        setup_log = KIT / '.setup.log'
        if setup_log.exists():
            log.write('\n--- .setup.log ---\n' + setup_log.read_text())
    if check and result.returncode != 0:
        raise RuntimeError(f'setup stages for {version} exited {result.returncode}; see {log_name}')
    pending = re.findall(r'^UPGRADE_PENDING=(.*)$', result.stdout, re.M)
    return result.returncode, (pending[-1].strip() if pending else None)


def env_value(key):
    for line in (KIT / '.env').read_text().splitlines():
        if line.startswith(key + '='):
            return line.split('=', 1)[1].strip().strip('"').strip("'")
    return ''


def set_env_value(key, value):
    lines = [l for l in (KIT / '.env').read_text().splitlines() if not l.startswith(key + '=')]
    (KIT / '.env').write_text('\n'.join(lines + [f'{key}={value}']) + '\n')


def installed():
    return {'release': (BIN / 'RELEASE').read_text().strip(), **{p: sha256(BIN / p) for p in PROGRAMS}}


def service_container(service):
    out = subprocess.run(['docker', 'ps', '-q', '--filter', f'label=com.docker.compose.project={PROJECT}',
        '--filter', f'label=com.docker.compose.service={service}'], capture_output=True, text=True, check=True)
    return out.stdout.strip()


def redis(*args):
    return subprocess.run(['docker', 'exec', service_container('redis'), 'redis-cli', *args],
        capture_output=True, text=True, check=True).stdout.strip()


def persistent_keys():
    keys = [k for k in redis('--scan').splitlines() if k]
    return sorted(k for k in keys if redis('TTL', k) == '-1')


def restart_stack(label):
    for service in ('redis', 'postgres', 'coder'):
        subprocess.run(['docker', 'restart', service_container(service)], check=True, capture_output=True)
    # Published ports can change on restart; the workspaces use network names instead.
    port = subprocess.run(['docker', 'port', service_container('coder'), '7080'],
        capture_output=True, text=True, check=True).stdout.splitlines()[0].strip()
    url = 'http://' + port.replace('0.0.0.0', '127.0.0.1')
    os.environ['CODER_URL'] = pb.CODER = url
    set_env_value('CODER_URL', url)
    def ready():
        try:
            return pb.coder('/buildinfo')
        except Exception:
            return False
    pb.wait(f'Coder did not come back after {label}', ready)
    step(f'Restarted Redis, PostgreSQL and Coder ({label}); Coder answers at its new address.')


def workspace(name):
    return next(w for w in pb.coder('/workspaces')['workspaces'] if w['name'] == name)


def controller_healthy(ssh_name):
    def healthy():
        try:
            return pb.ssh(ssh_name, 'curl -fsS http://localhost:3001/experimental/hooks/health')
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            return False
    pb.wait(f'controller in {ssh_name} is not healthy', healthy, 300)


def running_programs(ssh_name):
    out = pb.ssh(ssh_name, 'sha256sum /usr/local/bin/openflows')
    return out.split()[0]


def check_state(marker, keys, ssh_name, label):
    assert redis('GET', MARKER_KEY) == marker, f'Redis marker lost after {label}'
    missing = sorted(set(keys) - set(persistent_keys()))
    assert not missing, f'Redis keys lost after {label}: {missing}'
    controller_healthy(ssh_name)
    step(f'After {label}: Redis marker and {len(keys)} persistent keys intact, controller healthy.')


def wait_build(name, status):
    def done():
        w = workspace(name)
        build = w['latest_build']
        if build['job']['status'] in ('failed', 'canceled'):
            raise RuntimeError(f'{name} build failed: {build}')
        return w if build['status'] == status and build['job']['status'] == 'succeeded' else False
    return pb.wait(f'{name} did not reach {status}', done, 600)


def run():
    make_kit()
    os.environ['OPENFLOWS_RELEASE_BASE_URL'] = serve_releases()
    os.environ['TF_VAR_dev_binary_host_path'] = str(BIN)
    subprocess.run(['docker', 'tag', IMAGE[OLD], IMAGE[NEW]], check=True)
    old = publish(OLD)
    new = publish(NEW, marker=NEW)
    publish('9.2.1', marker='9.2.1', tamper=True)
    evidence['programs'] = {OLD: old, NEW: new}

    # 1. Install 9.0.0 and create a tenant on it.
    run_setup(OLD, 'stage_release', 'setup-install.log')
    assert installed() == {'release': OLD, **old}, installed()
    os.environ['PATH'] = f'{BIN}:{os.environ["PATH"]}'
    ws, templates, organization, ssh_name = pb.setup_tenant()
    name = ws['name']
    set_env_value('CODER_URL', pb.CODER)
    set_env_value('CODER_SESSION_TOKEN', os.environ['CODER_SESSION_TOKEN'])
    set_env_value('OPENFLOWS_SETUP_TEMPLATES_IMAGE', f'release:{OLD}')
    assert running_programs(ssh_name) == old['openflows']
    marker = str(uuid.uuid4())
    redis('SET', MARKER_KEY, marker)
    keys = persistent_keys()
    assert MARKER_KEY in keys
    step(f'Installed {OLD} from the setup kit; {name} runs its programs; {len(keys)} persistent Redis keys recorded.')

    # 2. Restart the stack.
    restart_stack('install')
    check_state(marker, keys, ssh_name, 'the first restart')

    # 3. Upgrade without consent: setup stops short and reports it.
    versions_before = {t['name']: t['active_version_id'] for t in templates}
    code, pending = run_setup(NEW, 'stage_release; stage_bootstrap; offer_update', 'setup-upgrade.log')
    assert installed() == {'release': NEW, **new}, installed()
    versions_after = {t['name']: t['active_version_id'] for t in pb.coder(f"/organizations/{organization['id']}/templates")}
    assert all(versions_after[t] != v for t, v in versions_before.items()), 'templates were not updated'
    assert workspace(name)['outdated'] is True
    assert pending and name in pending.split(), f'setup did not report {name} pending: {pending!r}'
    assert running_programs(ssh_name) == old['openflows'], 'the running workspace changed programs without consent'
    step(f'Upgrade to {NEW} without consent: programs and 5 templates replaced, {name} still runs {OLD}, '
         f'setup reports "{pending}" pending (exit 3).')

    # 4. Finish the upgrade.
    code, pending = run_setup(NEW, 'stage_bootstrap; offer_update', 'setup-upgrade-finish.log', update_workspaces=True)
    assert pending == '', f'upgrade still pending: {pending!r}'
    wait_build(name, 'running')
    assert workspace(name)['outdated'] is False
    controller_healthy(ssh_name)
    assert running_programs(ssh_name) == new['openflows'], f'{name} does not run {NEW}'
    check_state(marker, keys, ssh_name, f'the upgrade to {NEW}')
    step(f'With --update-workspaces, {name} moved to the {NEW} templates and runs {NEW}; nothing pending (exit 0).')

    # 5. Failed upgrades keep 9.1.0.
    for version, reason in (('9.2.0', 'missing release'), ('9.2.1', 'bad checksum')):
        code, _ = run_setup(version, 'stage_release', f'setup-failed-{version}.log', check=False)
        assert code != 0, f'setup accepted the {reason}'
        assert installed() == {'release': NEW, **new}, f'the {reason} changed the installed programs'
        assert running_programs(ssh_name) == new['openflows']
        step(f'Upgrade to {version} ({reason}) stopped with exit {code}; {NEW} still installed and running.')
    set_env_value('OPENFLOWS_VERSION', NEW)

    # 6. Roll back with the workspace stopped.
    pb.coder(f"/workspaces/{workspace(name)['id']}/builds", {'transition': 'stop'})
    wait_build(name, 'stopped')
    code, pending = run_setup(OLD, 'stage_release; stage_bootstrap; offer_update', 'setup-rollback.log')
    assert installed() == {'release': OLD, **old}, installed()
    stopped = workspace(name)
    assert stopped['latest_build']['status'] == 'stopped', 'rollback started a stopped workspace'
    assert stopped['outdated'] is False, 'the stopped workspace was not moved to the rolled-back templates'
    assert pending == '', f'rollback reported pending workspaces: {pending!r}'
    pb.coder(f"/workspaces/{stopped['id']}/builds", {'transition': 'start'})
    wait_build(name, 'running')
    controller_healthy(ssh_name)
    assert running_programs(ssh_name) == old['openflows'], f'{name} does not run {OLD} after rollback'
    check_state(marker, keys, ssh_name, f'the rollback to {OLD}')
    step(f'Rolled back to {OLD} with {name} stopped: it moved to the rolled-back templates, stayed stopped, '
         f'and runs {OLD} once started.')

    # 7. Restart after the round trip.
    restart_stack('rollback')
    check_state(marker, keys, ssh_name, 'the final restart')

    (ARTIFACTS / 'upgrade-evidence.json').write_text(json.dumps(evidence, indent=2))
    pb.coder(f"/workspaces/{workspace(name)['id']}/builds", {'transition': 'delete'})
    print('Upgrade, restart, failed upgrade and rollback of a running stack passed.', flush=True)


if __name__ == '__main__':
    try:
        run()
    finally:
        subprocess.run(['docker', 'image', 'rm', IMAGE[NEW]], capture_output=True)
