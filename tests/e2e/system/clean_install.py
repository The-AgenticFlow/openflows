"""Install one candidate release on a clean stack in one mode, then run it (#527).

The candidate is the programs this run built, packed as the release their version names:
an archive with its checksum and a setup kit (the files release-assets.yml packs), plus an
image holding the same programs, pushed to a throwaway local registry. OPENFLOWS_E2E_MODE
picks the mode:

  source   the programs built from this checkout (what `--source` compiles)
  binary   setup's stage_release, from the kit, downloads the archive from
           OPENFLOWS_RELEASE_BASE_URL and verifies its checksum
  compose  setup's stage_image, from the kit, pulls the image and pins its digest; the
           kit's openflows-binaries Compose service copies the programs out of it

Each mode must then pass openflows-doctor, start a tenant and run a command in its
workspace with the mode's own programs. clean-install-evidence.json records the platform,
the candidate's version, checksums and digest, and what each check returned.
"""
import json
import os
import platform
import subprocess
from pathlib import Path

import production_bootstrap as pb
import upgrade_rollback as kit

MODE = os.environ['OPENFLOWS_E2E_MODE']
# A release is named after the version its programs report.
VERSION = subprocess.run([str(kit.SCRATCH / 'bin' / 'openflows'), '--version'], capture_output=True,
    text=True, check=True).stdout.split()[-1]
ARTIFACTS = pb.ARTIFACTS
REGISTRY_IMAGE = 'registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373'
UBUNTU = 'ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55'
evidence = {'mode': MODE, 'version': VERSION, 'steps': []}


def step(text):
    print(text, flush=True)
    evidence['steps'].append(text)


def save():
    (ARTIFACTS / 'clean-install-evidence.json').write_text(json.dumps(evidence, indent=2))


def output(*command, **kwargs):
    return subprocess.run(command, capture_output=True, text=True, check=True, **kwargs).stdout.strip()


def platform_facts():
    facts = {'machine': platform.machine(), 'kernel': platform.release(),
             'docker': output('docker', 'version', '--format', '{{.Server.Version}}')}
    for line in Path('/etc/os-release').read_text().splitlines():
        if line.startswith('PRETTY_NAME='):
            facts['os'] = line.split('=', 1)[1].strip('"')
    for key in ('ImageOS', 'ImageVersion', 'RUNNER_OS', 'RUNNER_ARCH'):
        if os.environ.get(key):
            facts[key] = os.environ[key]
    return facts


def stages(stage_list, log_name):
    """Run setup.sh stages from the kit in this mode, as `./scripts/setup.sh --MODE` would."""
    script = f'''
        set -euo pipefail
        source "{kit.KIT}/scripts/setup.sh"
        MODE={MODE} REPO=test/repo
        env_set OPENFLOWS_SETUP_MODE {MODE}
        env_set OPENFLOWS_VERSION {VERSION}
        {stage_list}
    '''
    result = subprocess.run(['bash', '-c', script], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, timeout=900)
    setup_log = kit.KIT / '.setup.log'
    (ARTIFACTS / log_name).write_text(result.stdout + (
        '\n--- .setup.log ---\n' + setup_log.read_text() if setup_log.exists() else ''))
    if result.returncode != 0:
        raise RuntimeError(f'setup stages exited {result.returncode}; see {log_name}')


def publish_image(programs_dir):
    """Build an image holding the candidate's programs and push it to a local registry."""
    project = os.environ['OPENFLOWS_E2E_PROJECT']
    registry = f'{project}-registry'
    output('docker', 'run', '-d', '--name', registry, '-p', '127.0.0.1::5000', REGISTRY_IMAGE)
    port = output('docker', 'port', registry, '5000').splitlines()[0].rsplit(':', 1)[1]
    repo = f'localhost:{port}/openflows'
    context = kit.SCRATCH / 'image'
    context.mkdir()
    for program in kit.PROGRAMS:
        (context / program).write_bytes((programs_dir / program).read_bytes())
    (context / 'Dockerfile').write_text(
        f'FROM {UBUNTU}\n'
        f'LABEL org.opencontainers.image.version="{VERSION}"\n'
        f'COPY --chmod=0755 {" ".join(kit.PROGRAMS)} /usr/local/bin/\n')
    with (ARTIFACTS / 'candidate-image.log').open('w') as log:
        subprocess.run(['docker', 'build', '--platform', 'linux/amd64', '-t', f'{repo}:{VERSION}', str(context)],
            stdout=log, stderr=subprocess.STDOUT, check=True, timeout=600)
        subprocess.run(['docker', 'push', f'{repo}:{VERSION}'], stdout=log, stderr=subprocess.STDOUT,
            check=True, timeout=300)
        # setup must read the digest from the registry, not from this local build.
        subprocess.run(['docker', 'image', 'rm', f'{repo}:{VERSION}'], stdout=log, stderr=subprocess.STDOUT, check=True)
    return repo


def install():
    """Install the candidate in MODE; returns the folder the workspaces read programs from."""
    built = kit.SCRATCH / 'bin'
    candidate = kit.publish(VERSION)
    evidence['candidate_checksums'] = candidate
    if MODE == 'source':
        programs = built
        step(f'Source mode: using the programs built from this checkout ({kit.CHECKOUT}).')
    else:
        kit.make_kit()
        (kit.KIT / '.env').write_text(f'OPENFLOWS_SETUP_MODE={MODE}\n')
        programs = kit.BIN
    if MODE == 'binary':
        base = kit.serve_releases()
        os.environ['OPENFLOWS_RELEASE_BASE_URL'] = base
        stages('stage_release', 'setup-install.log')
        assert (programs / 'RELEASE').read_text().strip() == VERSION
        evidence['download_base'] = base
        step(f'Binary mode: setup downloaded {VERSION} from {base} and verified its checksum.')
    elif MODE == 'compose':
        repo = publish_image(built)
        os.environ['OPENFLOWS_IMAGE_REPO'] = repo
        stages('stage_image', 'setup-install.log')
        digest = kit.env_value('OPENFLOWS_IMAGE_DIGEST')
        assert digest.startswith('sha256:'), f'setup did not pin a digest: {digest!r}'
        compose = ['docker', 'compose', '-p', os.environ['OPENFLOWS_E2E_PROJECT'] + '-kit',
            '--env-file', str(kit.KIT / '.env'), '-f', str(kit.KIT / 'docker-compose.yml'),
            '-f', str(kit.KIT / 'docker-compose.images.yml')]
        with (ARTIFACTS / 'compose-binaries.log').open('w') as log:
            subprocess.run(compose + ['run', '--rm', '--no-deps', 'openflows-binaries'], cwd=kit.KIT,
                env={**os.environ, 'OPENFLOWS_IMAGE_REPO': repo}, stdout=log, stderr=subprocess.STDOUT,
                check=True, timeout=600)
        assert (programs / 'IMAGE').read_text().strip() == f'{repo}:{VERSION}@{digest}'
        evidence.update(image=f'{repo}:{VERSION}', digest=digest)
        step(f'Compose mode: setup pulled {repo}:{VERSION}, pinned {digest[:19]}..., and the kit\'s '
             f'openflows-binaries service installed the programs.')
    installed = {p: kit.sha256(programs / p) for p in kit.PROGRAMS}
    expected = {p: kit.sha256(built / p) for p in kit.PROGRAMS}
    assert installed == expected, f'{MODE} installed other programs: {installed} != {expected}'
    evidence['installed_checksums'] = installed
    version = output(str(programs / 'openflows'), '--version')
    assert version == f'openflows {VERSION}', f'installed programs report {version!r}, not {VERSION}'
    evidence['openflows_version_output'] = version
    step(f'Installed programs match the candidate; `openflows --version` prints "{version}".')
    return programs


def doctor(programs):
    result = subprocess.run([str(programs / 'openflows-doctor'), 'check'], stdin=subprocess.DEVNULL,
        capture_output=True, text=True, timeout=120)
    (ARTIFACTS / 'doctor.log').write_text(result.stdout + result.stderr)
    assert result.returncode == 0 and 'All checks passed' in result.stdout, \
        f'openflows-doctor failed (exit {result.returncode}); see doctor.log'
    assert 'LLM models configured in Coder' in result.stdout, 'openflows-doctor did not see the model'
    evidence['doctor'] = result.stdout.splitlines()
    step('openflows-doctor passed: Coder, LLM models and Redis checked.')


def run():
    evidence['platform'] = platform_facts()
    save()
    programs = install()
    save()
    os.environ['TF_VAR_dev_binary_host_path'] = str(programs)
    os.environ['PATH'] = f'{programs}:{os.environ["PATH"]}'
    ws, templates, _, ssh_name = pb.setup_tenant()
    step(f'Tenant started: {ws["name"]} cloned the repository and its controller is healthy.')
    doctor(programs)
    command = pb.ssh(ssh_name, 'set -eu; openflows --version; sha256sum /usr/local/bin/openflows')
    running = command.split()[-2]
    assert running == evidence['installed_checksums']['openflows'], \
        f'the workspace runs other programs: {running}'
    evidence['workspace_command'] = command
    step(f'Workspace command ran with the {MODE} programs: "{command.splitlines()[0]}".')
    evidence['templates'] = sorted(t['name'] for t in templates)
    save()
    pb.coder('/workspaces/' + ws['id'] + '/builds', {'transition': 'delete'})
    print(f'Clean install of {VERSION} in {MODE} mode passed.', flush=True)


if __name__ == '__main__':
    try:
        run()
    finally:
        save()
        project = os.environ['OPENFLOWS_E2E_PROJECT']
        subprocess.run(['docker', 'rm', '-f', f'{project}-registry'], capture_output=True)
        subprocess.run(['docker', 'compose', '-p', f'{project}-kit', 'down', '--remove-orphans'],
            capture_output=True)
