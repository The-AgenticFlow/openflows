"""Real Docker E2E for workspace-runtime v0.2.0 + Manager release pinning (#514/#515)."""

import hashlib
import http.cookiejar
import json
import os
import shutil
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

CHECKOUT = Path(os.environ["OPENFLOWS_E2E_CHECKOUT"])
SCRATCH = Path(os.environ["OPENFLOWS_E2E_SCRATCH"])
ARTIFACTS = Path(os.environ["OPENFLOWS_E2E_ARTIFACTS"])
PROJECT = os.environ["OPENFLOWS_E2E_PROJECT"]
CODER_URL = os.environ["CODER_URL"]
OAUTH_URL = os.environ["OPENFLOWS_E2E_OAUTH_URL"]
GITHUB_URL = os.environ["OPENFLOWS_E2E_GITHUB_URL"]
REDIS_URL = os.environ["REDIS_URL"]
DB_URL = os.environ["OPENFLOWS_E2E_MANAGER_DB_URL"]
TENANT = os.environ["OPENFLOWS_E2E_TENANT"]

V0_2_0_IMAGES = {
    "nexus": (
        "ghcr.io/the-agenticflow/workspace-runtime-nexus",
        "sha256:76db4d6953b4ed5ba64cfd47e155fdb7aca38cd79ee89b600f9f46457cbe12cd",
    ),
    "forge": (
        "ghcr.io/the-agenticflow/workspace-runtime-forge",
        "sha256:660f6f401a13103e2012997f03ab7d864f8853ce06b280b0371fb07d05a752f0",
    ),
    "sentinel": (
        "ghcr.io/the-agenticflow/workspace-runtime-sentinel",
        "sha256:b824305d32ad15f087cf00f17880d36ef21271623336de83d5db917a9517d07f",
    ),
    "vessel": (
        "ghcr.io/the-agenticflow/workspace-runtime-vessel",
        "sha256:59bff0379fa95382725b74196be274a9cbaac0e85e3183ffa6e5e48766342e3e",
    ),
    "lore": (
        "ghcr.io/the-agenticflow/workspace-runtime-lore",
        "sha256:5f42f72b37c9c8ec687599f9f905554ccf71e452d5a13e8b2133475c6fe068fb",
    ),
}


def build_manifest(runtime_version: str) -> str:
    return json.dumps(
        {
            "schema_version": 2,
            "runtime_version": runtime_version,
            "status": "candidate",
            "images": {
                role: {
                    "repository": repo,
                    "digest": digest,
                    "architectures": ["linux/amd64", "linux/arm64"],
                }
                for role, (repo, digest) in V0_2_0_IMAGES.items()
            },
            "roles": ["nexus", "forge", "sentinel", "vessel", "lore"],
            "bootstrap": {
                "contract_version": 2,
                "asset_install_command": "openflows-runtime install",
                "role_environment_variable": "OPENFLOWS_ROLE",
                "credential_file": "/run/openflows/runtime.json",
                "asset_directory": ".openflows/artifacts",
            },
            "harness": {
                "name": "openflows-harness",
                "openflows_release": "openflows-1.8.2",
            },
            "compatibility": {
                "openflows_template_parameter_schema_versions": [],
                "minimum_manager_version": None,
            },
        },
        indent=2,
    )


def wait(desc: str, fn, seconds: int = 180):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        res = fn()
        if res:
            return res
        time.sleep(2)
    raise TimeoutError(desc)


def run(cmd, env=None, check=True, timeout=60):
    return subprocess.run(
        cmd,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=check,
    )


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def http_json(url, method="GET", body=None, headers=None, opener=None):
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    open_fn = opener.open if opener else urllib.request.urlopen
    try:
        with open_fn(req, timeout=20) as resp:
            raw = resp.read()
            return resp.status, json.loads(raw) if raw else None, resp.headers
    except urllib.error.HTTPError as err:
        raw = err.read().decode(errors="replace")
        if 300 <= err.code < 400:
            return err.code, raw, err.headers
        raise RuntimeError(f"{method} {url}: HTTP {err.code}: {raw}") from err


def manager_env(secrets_dir: Path, manifest_path: Path, allow_candidate: bool, release_name: str, public_url: str, runtime_sha: str | None = None, oauth_client_id: str | None = None):
    env = {
        "OPENFLOWS_MODE": "local",
        "OPENFLOWS_CONNECTED_LOCAL_ENABLED": "true",
        "OPENFLOWS_RELEASE_NAME": release_name,
        "OPENFLOWS_TEMPLATE_SOURCE_DIR": "/opt/openflows-templates",
        "OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST": str(manifest_path),
        "OPENFLOWS_WORKSPACE_RUNTIME_ALLOW_CANDIDATE": "true" if allow_candidate else "false",
        "OPENFLOWS_DATABASE_URL": "postgres://openflows:openflows@openflows-db:5432/openflows_control_plane",
        "OPENFLOWS_REDIS_ADMIN_URL": "redis://redis:6379",
        "OPENFLOWS_RUNTIME_REDIS_URL": "redis://redis:6379",
        "REDIS_URL": "redis://redis:6379",
        "CODER_URL": "http://coder:7080",
        "OPENFLOWS_CODER_BUILTIN_PROVISIONER": "1",
        "OPENFLOWS_CODER_PUBLIC_URL": CODER_URL,
        "OPENFLOWS_PUBLIC_URL": public_url,
        "OPENFLOWS_RUNTIME_MANAGER_URL": "http://openflows-manager:3002",
        "OPENFLOWS_RUNTIME_RELAY_BASE_URL": "http://runtime-relay:8080/tenants/{tenant}",
        "OPENFLOWS_HOOK_URL": "http://openflows-manager:3002/api/v1/hooks/coder/chat",
        "OPENFLOWS_AUTH_ALLOW_LOOPBACK": "true",
        "OPENFLOWS_AUTH_COOKIE_SECURE": "false",
        "OPENFLOWS_SECRET_PROVIDER": "filesystem",
        "OPENFLOWS_SECRET_DIR": "/run/openflows-local/secrets",
        "OPENFLOWS_CONNECTED_LOCAL_SECRET_DIR": str(secrets_dir),
        "OPENFLOWS_PROVISIONER_SECRET_DIR": str(secrets_dir / "provisioners"),
        "OPENFLOWS_PROVISIONER_NETWORK": f"{PROJECT}_default",
        "OPENFLOWS_CRYPTO_MASTER_KEY_REF": "master-key",
        "OPENFLOWS_CODER_OPERATOR_TOKEN_REF": "coder-operator-token",
        "OPENFLOWS_CODER_HOOK_SECRET_REF": "coder-hook-secret",
        "OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF": "coder-oauth-client-secret",
        "OPENFLOWS_GITHUB_CLIENT_ID": "ci-coder",
        "OPENFLOWS_GITHUB_APP_SLUG": "openflows-local",
        "OPENFLOWS_GITHUB_CLIENT_SECRET_REF": "github-client-secret",
        "OPENFLOWS_GITHUB_API_BASE": "http://github-fixture:8080",
        "OPENFLOWS_GITHUB_OAUTH_BASE": "http://oauth:8080",
        "OPENFLOWS_CONNECTED_LOCAL_CREDENTIAL_FILE": "/run/openflows-local/github-user.json",
        "OPENFLOWS_LOCAL_BROKER_KEY_FILE": "/run/openflows-local/broker-key",
        "TF_VAR_docker_network": os.environ["TF_VAR_docker_network"],
        "TF_VAR_github_api_base": os.environ["TF_VAR_github_api_base"],
        "TF_VAR_github_git_base": os.environ["TF_VAR_github_git_base"],
        "TF_VAR_connected_local": os.environ.get("TF_VAR_connected_local", "true"),
        "TF_VAR_connected_local_broker_secret_host_path": os.environ["TF_VAR_connected_local_broker_secret_host_path"],
        "TF_VAR_connected_local_helper_host_path": os.environ["TF_VAR_connected_local_helper_host_path"],
        "TF_VAR_connected_local_manager_url": os.environ["TF_VAR_connected_local_manager_url"],
        "OPENFLOWS_MANAGER_ADDR": "0.0.0.0:3002",
        "RUST_LOG": "openflows_manager=debug,tower_http=info",
    }
    if runtime_sha is not None:
        env["OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST_SHA256"] = runtime_sha
    if oauth_client_id is not None:
        env["OPENFLOWS_CODER_OAUTH_CLIENT_ID"] = oauth_client_id
    return env


def start_manager_container(name: str, secrets_dir: Path, env: dict[str, str]):
    cmd = [
        "docker", "run", "-d",
        "--name", name,
        "--network", f"{PROJECT}_default",
        "--network-alias", "openflows-manager",
        "-p", "127.0.0.1::3002",
        "-v", f"{SCRATCH}/bin/openflows-manager:/usr/local/bin/openflows-manager:ro",
        "-v", f"{CHECKOUT}/crates/coder-client/templates:/opt/openflows-templates:ro",
        "-v", f"{secrets_dir}:/run/openflows-local",
        "-v", f"{secrets_dir / 'provisioners'}:{secrets_dir / 'provisioners'}",
    ]
    for k, v in env.items():
        cmd.extend(["-e", f"{k}={v}"])
    cmd.extend([os.environ["OPENFLOWS_SYSTEM_FIXTURE_IMAGE"], "openflows-manager", "serve"])
    cid = run(cmd).stdout.strip()
    port_line = run(["docker", "port", cid, "3002/tcp"]).stdout.strip().splitlines()[0]
    port = port_line.rsplit(":", 1)[1]
    base_url = f"http://127.0.0.1:{port}"
    wait(f"Manager {name} ready", lambda: _ready(base_url), seconds=60)
    return cid, base_url


def _ready(base_url: str) -> bool:
    try:
        st, _, _ = http_json(f"{base_url}/ready")
        return st == 200
    except Exception:
        return False


def _coder_ready() -> bool:
    try:
        st, _, _ = http_json(f"{CODER_URL}/api/v2/buildinfo")
        return st == 200
    except Exception:
        return False


def stop_manager_container(cid: str, log_name: str):
    logs = run(["docker", "logs", cid], check=False)
    (ARTIFACTS / log_name).write_text(logs.stdout + logs.stderr)
    run(["docker", "rm", "-f", cid], check=False)


def assert_no_workspaces():
    res = run(["docker", "ps", "-aq", "--filter", f"network={PROJECT}_default", "--filter", "name=of-"])
    assert not res.stdout.strip(), f"Unexpected workspace containers exist: {res.stdout}"


def main():
    secrets_dir = SCRATCH / "manager-local"
    (secrets_dir / "secrets").mkdir(parents=True, exist_ok=True)
    (secrets_dir / "provisioners").mkdir(parents=True, exist_ok=True)
    secrets_dir.chmod(0o700)
    (secrets_dir / "secrets" / "github-client-secret").write_text("disposable-ci-client-secret")
    broker_key = "a" * 64
    (SCRATCH / "broker-key").write_text(broker_key)
    (SCRATCH / "broker-key").chmod(0o600)
    (secrets_dir / "github-broker-key").write_text(broker_key)
    (secrets_dir / "github-broker-key").chmod(0o600)

    manifest_v020 = secrets_dir / "v0.2.0-manifest.json"
    manifest_v020.write_text(build_manifest("0.2.0"))
    manifest_v021 = secrets_dir / "v0.2.1-manifest.json"
    manifest_v021.write_text(build_manifest("0.2.1"))

    # 0. Wait for Coder HTTP server to be ready.
    wait("Coder HTTP server ready", lambda: _coder_ready(), seconds=120)

    # 1. Run `openflows-manager setup --initialize-coder` inside the compose network so CODER_URL=http://coder:7080 matches `serve`.
    setup_cmd = [
        "docker", "run", "--rm",
        "--network", f"{PROJECT}_default",
        "-v", f"{SCRATCH}/bin/openflows-manager:/usr/local/bin/openflows-manager:ro",
        "-v", f"{secrets_dir}:/run/openflows-local",
        "-e", "OPENFLOWS_MODE=local",
        "-e", "OPENFLOWS_SECRET_PROVIDER=filesystem",
        "-e", "OPENFLOWS_SECRET_DIR=/run/openflows-local/secrets",
        "-e", "OPENFLOWS_CRYPTO_MASTER_KEY_REF=master-key",
        "-e", "OPENFLOWS_CODER_OPERATOR_TOKEN_REF=coder-operator-token",
        "-e", "OPENFLOWS_CODER_HOOK_SECRET_REF=coder-hook-secret",
        "-e", "OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF=coder-oauth-client-secret",
        "-e", "OPENFLOWS_PUBLIC_URL=http://127.0.0.1:3002",
        "-e", "CODER_URL=http://coder:7080",
        os.environ["OPENFLOWS_SYSTEM_FIXTURE_IMAGE"],
        "openflows-manager", "setup", "--initialize-coder", "--admin-email", "ci@example.test",
    ]
    setup_res = run(setup_cmd, check=False, timeout=90)
    (ARTIFACTS / "manager-setup.log").write_text(setup_res.stdout + setup_res.stderr)
    assert setup_res.returncode == 0, (
        f"openflows-manager setup failed ({setup_res.returncode}):\n"
        f"{setup_res.stdout}\n{setup_res.stderr}"
    )
    print("1. Manager setup --initialize-coder succeeded.", flush=True)

    # 2. #514 negative checks: candidate without opt-in & bad sha256 rejected BEFORE any workspace.
    for label, mpath, allow, sha in [
        ("candidate-rejected", "/run/openflows-local/v0.2.0-manifest.json", False, None),
        ("bad-sha256", "/run/openflows-local/v0.2.0-manifest.json", True, "0" * 64),
        ("missing-manifest", "/run/openflows-local/does-not-exist.json", True, None),
    ]:
        env = manager_env(secrets_dir, Path(mpath), allow, "2026.11.0-bad", "http://127.0.0.1:3002", sha)
        cmd = [
            "docker", "run", "--rm",
            "--network", f"{PROJECT}_default",
            "-v", f"{SCRATCH}/bin/openflows-manager:/usr/local/bin/openflows-manager:ro",
            "-v", f"{CHECKOUT}/crates/coder-client/templates:/opt/openflows-templates:ro",
            "-v", f"{secrets_dir}:/run/openflows-local",
        ]
        for k, v in env.items():
            cmd.extend(["-e", f"{k}={v}"])
        cmd.extend([os.environ["OPENFLOWS_SYSTEM_FIXTURE_IMAGE"], "openflows-manager", "serve"])
        res = run(cmd, check=False, timeout=30)
        assert res.returncode != 0, f"Expected failure for {label}, got 0"
        assert_no_workspaces()
    print("2. #514 pre-provisioning rejection checks passed with 0 workspaces created.", flush=True)

    deployment_json = json.loads(run([
        "docker", "run", "--rm",
        "-v", f"{secrets_dir}:/run/openflows-local:ro",
        os.environ["OPENFLOWS_SYSTEM_FIXTURE_IMAGE"],
        "cat", "/run/openflows-local/secrets/deployment.json",
    ]).stdout)
    oauth_client_id = deployment_json["oauth_id"]

    # 3. Start Manager with v0.2.0 manifest.
    mgr_name = f"{PROJECT}-manager"
    env_v020 = manager_env(
        secrets_dir,
        Path("/run/openflows-local/v0.2.0-manifest.json"),
        True,
        "2026.11.0-runtime-0.2.0",
        "http://127.0.0.1:3002",
        oauth_client_id=oauth_client_id,
    )
    mgr_cid = None
    mgr_cid, mgr_url = start_manager_container(mgr_name, secrets_dir, env_v020)
    try:
        # 4. Authenticate in connected-local mode via /api/v1/local/github/device -> /poll (sets of_session cookie).
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar), NoRedirect())
        _, ldev, _ = http_json(
            f"{mgr_url}/api/v1/local/github/device",
            method="POST",
            body={},
            headers={"X-OpenFlows-Local-Console": "1"},
            opener=opener,
        )
        http_json(
            f"{OAUTH_URL}/approve",
            method="POST",
            body={"user_code": ldev["user_code"]},
            headers={"Authorization": "Bearer ci-operator-token"},
        )
        wait("local github device poll", lambda: _poll_local_gh(mgr_url, opener, ldev["device_code"]), seconds=30)
        _, csrf_data, _ = http_json(f"{mgr_url}/api/v1/auth/csrf", opener=opener)
        csrf_token = csrf_data["csrf_token"]

        # Exchange session for Bearer access token via CLI device flow and grant platform operator role.
        bearer = _issue_cli_bearer(mgr_url, opener, csrf_token)
        _grant_platform_operator(mgr_url, bearer)

        # Link Coder identity via Coder admin session.
        coder_pw = run([
            "docker", "run", "--rm",
            "-v", f"{secrets_dir}:/run/openflows-local:ro",
            os.environ["OPENFLOWS_SYSTEM_FIXTURE_IMAGE"],
            "cat", "/run/openflows-local/secrets/coder-bootstrap-password",
        ]).stdout.strip()
        coder_login = http_json(
            f"{CODER_URL}/api/v2/users/login",
            method="POST",
            body={"email": "ci@example.test", "password": coder_pw},
        )[1]
        admin_coder_token = coder_login["session_token"]
        _, coder_orgs, _ = http_json(
            f"{CODER_URL}/api/v2/organizations",
            headers={"Coder-Session-Token": admin_coder_token},
        )
        default_coder_org = coder_orgs[0]["id"]
        _, gh_coder_user, _ = http_json(
            f"{CODER_URL}/api/v2/users",
            method="POST",
            body={
                "email": "operator@example.test",
                "username": "operator",
                "login_type": "github",
                "organization_ids": [default_coder_org],
            },
            headers={"Coder-Session-Token": admin_coder_token},
        )
        http_json(
            f"{CODER_URL}/api/v2/users/{gh_coder_user['id']}/status/activate",
            method="PUT",
            body={},
            headers={"Coder-Session-Token": admin_coder_token},
        )
        _, gh_coder_key, _ = http_json(
            f"{CODER_URL}/api/v2/users/{gh_coder_user['id']}/keys/tokens",
            method="POST",
            body={"lifetime": 3600000000000, "scope": "all"},
            headers={"Coder-Session-Token": admin_coder_token},
        )
        coder_token = gh_coder_key["key"]
        _, coder_start, _ = http_json(
            f"{mgr_url}/api/v1/auth/coder/start",
            method="POST",
            body={},
            headers={"X-CSRF-Token": csrf_token},
            opener=opener,
        )
        coder_opener = urllib.request.build_opener(NoRedirect())
        auth_url = coder_start["authorization_url"]
        if auth_url.startswith("http://coder:7080"):
            auth_url = CODER_URL + auth_url[len("http://coder:7080"):]
        req_auth = urllib.request.Request(
            auth_url,
            data=urllib.parse.urlsplit(auth_url).query.encode(),
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                "Cookie": f"coder_session_token={coder_token}",
                "Coder-Session-Token": coder_token,
            },
            method="POST",
        )
        try:
            with coder_opener.open(req_auth, timeout=20) as resp:
                raise AssertionError(f"Expected redirect from POST /oauth2/authorize, got {resp.status}")
        except urllib.error.HTTPError as err:
            assert err.code in (302, 303, 307), f"Expected redirect from POST /oauth2/authorize, got {err.code}: {err.read().decode(errors='replace')}"
            chdrs = err.headers
        cb_url = chdrs["Location"]
        if cb_url.startswith("http://127.0.0.1:3002"):
            cb_url = mgr_url + cb_url[len("http://127.0.0.1:3002"):]
        st, _, _ = http_json(cb_url, opener=opener)
        assert st in (200, 302, 303)
        print("3. Authenticated Manager user + linked local GitHub + linked Coder identity.", flush=True)

        # 5. Create organization, bind Coder OSS default org, and wait for provisioning + v0.2.0 pinning.
        default_coder_org_id = _coder_default_org_id()
        _, org_res, _ = http_json(
            f"{mgr_url}/api/v1/organizations",
            method="POST",
            body={"slug": "e2e-runtime-org", "display_name": "e2e-runtime-org"},
            headers={**bearer, "Idempotency-Key": "org-create-1"},
        )
        org_id = org_res["resource_id"]
        org_op_id = org_res["operation_id"]
        _bind_coder_org_id(org_id, default_coder_org_id)
        wait(
            "Organization provisioned",
            lambda: _op_succeeded(mgr_url, bearer, org_id, org_op_id),
            seconds=600,
        )
        rel_v020 = _org_current_release(org_id)
        assert rel_v020, "Organization must be pinned to v0.2.0 release after provisioning"
        print(f"4. Organization {org_id} provisioned and pinned to v0.2.0 release {rel_v020}.", flush=True)

        # 6. Create tenant and wait for Nexus running + healthy runtime heartbeat.
        bearer = _issue_cli_bearer(mgr_url, opener, csrf_token)
        _, ten_res, _ = http_json(
            f"{mgr_url}/api/v1/organizations/{org_id}/tenants",
            method="POST",
            body={"repository_url": "https://github.com/test/repo", "name": TENANT, "fleet": 1},
            headers={**bearer, "Idempotency-Key": "tenant-create-1"},
        )
        tenant_id = ten_res["tenant_id"]
        wait(
            "Tenant Nexus running and healthy",
            lambda: _tenant_running(mgr_url, bearer, org_id, tenant_id),
            seconds=360,
        )
        print(f"5. Tenant {tenant_id} Nexus workspace is running and healthy.", flush=True)

        # 7. Find Nexus container & verify runtime assertions inside Nexus.
        nexus_cid = _find_container("openflows-nexus-")
        _assert_workspace_runtime(nexus_cid, "nexus", V0_2_0_IMAGES["nexus"])
        print("6. Verified Nexus workspace-runtime v0.2.0 assertions.", flush=True)

        # 8. Spawn Forge via Manager runtime API using Nexus's registered runtime token & verify Forge + relay.
        _spawn_forge_from_nexus(nexus_cid, "T-1", "forge/T-1")
        wait("Forge container started", lambda: len(_find_containers("openflows-forge-")) == 1, seconds=180)
        forge_cid = _find_container("openflows-forge-")
        wait("Forge workspace initialized", lambda: _forge_ready(forge_cid), seconds=180)
        _assert_workspace_runtime(forge_cid, "forge", V0_2_0_IMAGES["forge"])
        _assert_nexus_forge_relay(nexus_cid, forge_cid)
        print("7. Verified Forge workspace-runtime v0.2.0, golden repo seed, and Nexus<->Forge relay.", flush=True)

        # 9. #514 Upgrade & rollback: restart Manager with v0.2.1, verify existing org/tenant stays pinned until promoted.
        stop_manager_container(mgr_cid, "manager-v020.log")
        env_v021 = manager_env(
            secrets_dir,
            Path("/run/openflows-local/v0.2.1-manifest.json"),
            True,
            "2026.11.1-runtime-0.2.1",
            "http://127.0.0.1:3002",
            oauth_client_id=oauth_client_id,
        )
        mgr_cid, mgr_url = start_manager_container(mgr_name, secrets_dir, env_v021)
        bearer = _issue_cli_bearer(mgr_url, opener, csrf_token)
        assert _org_current_release(org_id) == rel_v020, "Existing org must stay pinned before promotion"

        _, promo2, _ = http_json(
            f"{mgr_url}/api/v1/platform/organizations/{org_id}/template-release/promote-configured",
            method="POST",
            body={},
            headers=bearer,
        )
        rel_v021 = promo2["release_id"]
        assert rel_v021 != rel_v020
        wait(
            "Promotion to v0.2.1",
            lambda: _op_succeeded(mgr_url, bearer, org_id, promo2["operation_id"]),
            seconds=600,
        )
        assert _org_current_release(org_id) == rel_v021
        print(f"8. Explicit upgrade promoted {rel_v021}; rolling back to {rel_v020}.", flush=True)

        stop_manager_container(mgr_cid, "manager-v021.log")
        mgr_cid = None
        mgr_cid, mgr_url = start_manager_container(mgr_name, secrets_dir, env_v020)
        bearer = _issue_cli_bearer(mgr_url, opener, csrf_token)
        _, promo_rb, _ = http_json(
            f"{mgr_url}/api/v1/platform/organizations/{org_id}/template-release/promote-configured",
            method="POST",
            body={},
            headers=bearer,
        )
        assert promo_rb["release_id"] == rel_v020
        wait(
            "Rollback to v0.2.0",
            lambda: _op_succeeded(mgr_url, bearer, org_id, promo_rb["operation_id"]),
            seconds=600,
        )
        assert _org_current_release(org_id) == rel_v020
        print("9. Rollback to v0.2.0 release verified.", flush=True)
    finally:
        coder_logs = run(["docker", "logs", f"{PROJECT}-coder-1"], check=False)
        (ARTIFACTS / "coder.log").write_text(coder_logs.stdout + coder_logs.stderr)
        for cid in _find_containers("openflows-nexus-") + _find_containers("openflows-forge-"):
            cname = run(["docker", "inspect", "--format", "{{.Name}}", cid], check=False).stdout.strip().lstrip("/") or cid
            wlogs = run(["docker", "logs", cid], check=False)
            (ARTIFACTS / f"{cname}.log").write_text(wlogs.stdout + wlogs.stderr)
        if mgr_cid:
            stop_manager_container(mgr_cid, "manager-final.log")


def _issue_cli_bearer(mgr_url: str, opener, csrf_token: str) -> dict[str, str]:
    run([
        "docker", "exec", f"{PROJECT}-openflows-db-1",
        "psql", "-U", "openflows", "-d", "openflows_control_plane", "-tAc",
        "UPDATE sessions SET last_authenticated_at = clock_timestamp() WHERE revoked_at IS NULL",
    ])
    _, cli_dev, _ = http_json(f"{mgr_url}/api/v1/auth/cli/start", method="POST", body={})
    approve_data = urllib.parse.urlencode({"code": cli_dev["user_code"], "_csrf": csrf_token}).encode()
    approve_req = urllib.request.Request(
        f"{mgr_url}/auth/cli/approve",
        data=approve_data,
        headers={
            "Content-Type": "application/x-www-form-urlencoded",
            "X-CSRF-Token": csrf_token,
        },
        method="POST",
    )
    with opener.open(approve_req, timeout=20) as resp:
        assert resp.status == 200, f"Expected 200 from /auth/cli/approve, got {resp.status}"
    _, tok, _ = http_json(
        f"{mgr_url}/api/v1/auth/cli/token",
        method="POST",
        body={"device_code": cli_dev["device_code"]},
    )
    assert tok.get("status") == "authorized" and tok.get("access_token"), f"Unexpected token response: {tok}"
    return {"Authorization": f"Bearer {tok['access_token']}"}


def _poll_local_gh(mgr_url, opener, device_code):
    try:
        st, data, _ = http_json(
            f"{mgr_url}/api/v1/local/github/device/poll",
            method="POST",
            body={"device_code": device_code},
            headers={"X-OpenFlows-Local-Console": "1"},
            opener=opener,
        )
        return st == 200 and data and data.get("status") == "authorized"
    except Exception as exc:
        print(f"poll_local_gh error: {exc}", flush=True)
        return False


def _grant_platform_operator(mgr_url: str, bearer: dict[str, str]):
    _, me, _ = http_json(f"{mgr_url}/api/v1/me", headers=bearer)
    user_id = me["id"]
    run([
        "docker", "exec", f"{PROJECT}-openflows-db-1",
        "psql", "-U", "openflows", "-d", "openflows_control_plane", "-tAc",
        f"INSERT INTO platform_operators (user_id, created_by) VALUES ('{user_id}', '{user_id}') ON CONFLICT (user_id) DO NOTHING",
    ])


def _coder_default_org_id() -> str:
    res = run([
        "docker", "exec", f"{PROJECT}-postgres-1",
        "psql", "-U", "coder", "-d", "coder", "-tAc",
        "SELECT id::text FROM organizations WHERE is_default = true LIMIT 1",
    ])
    org_id = res.stdout.strip()
    assert org_id, "Could not resolve default Coder organization id"
    return org_id


def _bind_coder_org_id(org_id: str, coder_org_id: str):
    run([
        "docker", "exec", f"{PROJECT}-openflows-db-1",
        "psql", "-U", "openflows", "-d", "openflows_control_plane", "-tAc",
        f"UPDATE organizations SET coder_organization_id = '{coder_org_id}' WHERE id = '{org_id}'",
    ])


def _op_succeeded(mgr_url, bearer, _org_id, op_id):
    st, data, _ = http_json(
        f"{mgr_url}/api/v1/operations/{op_id}",
        headers=bearer,
    )
    if st != 200 or not data:
        return False
    state = data.get("state")
    if state == "retryable_failed":
        http_json(
            f"{mgr_url}/api/v1/operations/{op_id}/retry",
            method="POST",
            body={},
            headers={**bearer, "Idempotency-Key": f"retry-{op_id}-{int(time.time() * 1000)}"},
        )
        return False
    if state in ("failed", "terminal_failed", "cancelled"):
        raise AssertionError(f"Operation {op_id} failed: {data}")
    return state == "succeeded"


def _org_current_release(org_id: str) -> str:
    res = run([
        "docker", "exec", f"{PROJECT}-openflows-db-1",
        "psql", "-U", "openflows", "-d", "openflows_control_plane", "-tAc",
        f"SELECT release_id::text FROM organization_current_releases WHERE organization_id = '{org_id}'",
    ])
    return res.stdout.strip()


_LAST_TENANT_STATUS = None


def _tenant_running(mgr_url, bearer, org_id, tenant_id):
    global _LAST_TENANT_STATUS
    _, status, _ = http_json(
        f"{mgr_url}/api/v1/organizations/{org_id}/tenants/{tenant_id}/status",
        headers=bearer,
    )
    snap = json.dumps(status, sort_keys=True)
    if snap != _LAST_TENANT_STATUS:
        _LAST_TENANT_STATUS = snap
        print(f"   tenant status: {snap}", flush=True)
    op = status.get("latest_operation") or {}
    if op.get("state") in ("failed", "terminal_failed", "cancelled"):
        raise AssertionError(f"Tenant operation failed: {op}")
    return status.get("runtime", {}).get("health") == "healthy"


def _find_containers(prefix: str) -> list[str]:
    res = run(["docker", "ps", "-q", "--filter", f"network={PROJECT}_default", "--filter", f"name={prefix}"])
    return [c for c in res.stdout.strip().splitlines() if c]


def _find_container(prefix: str) -> str:
    cids = _find_containers(prefix)
    assert len(cids) == 1, f"Expected 1 container matching {prefix}, got {cids}"
    return cids[0]


def _spawn_forge_from_nexus(nexus_cid: str, ticket_id: str, branch: str):
    py = (
        "import glob, json, os, urllib.request\n"
        "token = None\n"
        "mgr_url = 'http://openflows-manager:8080'\n"
        "for p in glob.glob('/proc/[0-9]*/environ'):\n"
        "    try:\n"
        "        raw = open(p, 'rb').read().split(b'\\0')\n"
        "    except OSError:\n"
        "        continue\n"
        "    env = dict(kv.decode('utf-8', 'ignore').split('=', 1) for kv in raw if b'=' in kv)\n"
        "    if 'OF_RUNTIME_ACCESS_TOKEN' in env:\n"
        "        token = env['OF_RUNTIME_ACCESS_TOKEN']\n"
        "        mgr_url = env.get('OF_MANAGER_URL', mgr_url)\n"
        "        break\n"
        "assert token, 'OF_RUNTIME_ACCESS_TOKEN not found in Nexus controller process'\n"
        "req = urllib.request.Request(\n"
        "    f'{mgr_url}/api/v1/runtime/workspaces',\n"
        "    data=json.dumps({'role': 'forge', 'slot': 1, 'ticket_id': %r, 'branch': %r}).encode(),\n"
        "    headers={'Authorization': f'Bearer {token}', 'Content-Type': 'application/json', 'Idempotency-Key': 'forge-spawn-1'},\n"
        "    method='POST',\n"
        ")\n"
        "with urllib.request.urlopen(req, timeout=30) as resp:\n"
        "    print(resp.status, resp.read().decode())\n"
    ) % (ticket_id, branch)
    run(["docker", "exec", "-u", "coder", nexus_cid, "python3", "-c", py])


def _forge_ready(forge_cid: str) -> bool:
    res = subprocess.run(
        [
            "docker", "exec", "-u", "coder", forge_cid, "bash", "-lc",
            "test -d /home/coder/workspace/.git && test -f /home/coder/workspace/answer.txt && test -f /home/coder/workspace/AGENTS.md && test -d /home/coder/workspace/.agents/skills/shared-harness-protocol && OPENFLOWS_ROLE=forge openflows-runtime check >/dev/null 2>&1",
        ],
        capture_output=True,
        text=True,
    )
    return res.returncode == 0


def _assert_workspace_runtime(cid: str, role: str, expected_image: tuple[str, str]):
    repo, digest = expected_image
    expected_ref = f"{repo}@{digest}"
    image_id = run(["docker", "inspect", "--format", "{{.Image}}", cid]).stdout.strip()
    repo_digests = json.loads(run(["docker", "inspect", "--format", "{{json .RepoDigests}}", image_id]).stdout)
    assert expected_ref in repo_digests, f"{role} image {repo_digests} missing pinned {expected_ref}"

    check_cmd = (
        f"set -eu; "
        f"OPENFLOWS_ROLE={role} openflows-runtime check; "
        f"test -x /usr/local/bin/openflows-harness; "
        f"test -f /home/coder/.openflows/artifacts/orchestration/agent/agents/{role}.agent.md; "
        f"test -d /home/coder/.openflows/artifacts/plugin/skills/shared-harness-protocol; "
        f"(test -r /run/openflows/runtime.json || sudo test -r /run/openflows/runtime.json); "
    )
    if role == "nexus":
        check_cmd += "test -d /home/coder/.openflows/golden/repo/.git; "
    elif role == "forge":
        check_cmd += (
            "test -d /home/coder/.openflows/golden/repo/.git; "
            "test -d /home/coder/workspace/.git; "
            "test \"$(cat /home/coder/workspace/answer.txt)\" = '41'; "
            "test -f /home/coder/workspace/AGENTS.md; "
            "test -d /home/coder/workspace/.agents/skills/shared-harness-protocol; "
        )
    run(["docker", "exec", "-u", "coder", cid, "bash", "-lc", check_cmd])


def _assert_nexus_forge_relay(nexus_cid: str, forge_cid: str):
    nexus_name = run(["docker", "inspect", "--format", "{{.Name}}", nexus_cid]).stdout.strip().lstrip("/")
    check_cmd = (
        f"set -eu; "
        f"curl -fsS http://{nexus_name}:3000/health >/dev/null; "
        f"curl -fsS http://openflows-nexus:3000/health >/dev/null; "
        f"python3 -c \"import socket; s=socket.create_connection(('redis', 6379), 5); s.sendall(b'PING\\\\r\\\\n'); assert b'+PONG' in s.recv(64)\""
    )
    run(["docker", "exec", "-u", "coder", forge_cid, "bash", "-lc", check_cmd])


if __name__ == "__main__":
    main()
