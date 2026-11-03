#!/usr/bin/env python3
"""Create the Coder API token used by the OpenFlows Manager (local setup).

Reads CODER_SESSION_TOKEN from the process environment or the repository .env,
then stores a separately minted token in OPENFLOWS_DEV_SECRETS_FILE under
OPENFLOWS_CODER_OPERATOR_TOKEN_REF from .env.prod. Token values are never
printed. This is a local development helper; production deployments should
provision the token through their secret manager/bootstrap pipeline.
"""

from __future__ import annotations

import json
import os
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parent.parent


def read_env(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:].lstrip()
        key, separator, value = line.partition("=")
        if not separator or not key.strip():
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[key.strip()] = value
    return values


def required(values: dict[str, str], key: str) -> str:
    value = os.environ.get(key) or values.get(key, "")
    if not value.strip():
        raise RuntimeError(f"required setting {key} is missing")
    return value.strip()


def request_json(base_url: str, path: str, token: str, body: dict | None = None) -> dict | list:
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {"Accept": "application/json", "Coder-Session-Token": token}
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = Request(base_url.rstrip("/") + path, data=data, headers=headers,
                      method="GET" if data is None else "POST")
    try:
        with urlopen(request, timeout=15) as response:
            return json.loads(response.read())
    except HTTPError as error:
        raise RuntimeError(f"Coder API request failed with HTTP {error.code}: {path}") from None
    except (URLError, TimeoutError, json.JSONDecodeError) as error:
        raise RuntimeError(f"Coder API request failed for {path}: {type(error).__name__}") from None


def valid_bootstrap_token(base_url: str, token: str) -> bool:
    try:
        request_json(base_url, "/api/v2/users/me", token)
        return True
    except RuntimeError:
        return False


def cli_token(base_url: str) -> str:
    """Read the Coder CLI's keyring token without writing it to terminal output."""
    try:
        result = subprocess.run(
            ["coder", "--url", base_url, "login", "token"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return result.stdout.strip() if result.returncode == 0 else ""


def atomic_write_secrets(path: Path, values: dict[str, str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=".manager-secrets.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            json.dump(values, output, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def main() -> int:
    manager_env = read_env(ROOT / ".env.prod")
    legacy_env = read_env(ROOT / ".env")
    base_url = required(manager_env, "CODER_URL")
    token_ref = required(manager_env, "OPENFLOWS_CODER_OPERATOR_TOKEN_REF")
    secrets_file = Path(required(manager_env, "OPENFLOWS_DEV_SECRETS_FILE")).expanduser()
    if not secrets_file.is_absolute():
        secrets_file = ROOT / secrets_file

    parsed = urlsplit(base_url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise RuntimeError("CODER_URL must be an http(s) URL")
    if parsed.scheme == "http" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}:
        raise RuntimeError("HTTP is allowed only for a loopback Coder URL; use HTTPS otherwise")

    if not secrets_file.is_file():
        raise RuntimeError(f"manager secrets file is missing: {secrets_file}")
    try:
        secret_values = json.loads(secrets_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raise RuntimeError("manager secrets file is unreadable or invalid JSON") from None
    if not isinstance(secret_values, dict) or not all(
        isinstance(key, str) and isinstance(value, str)
        for key, value in secret_values.items()
    ):
        raise RuntimeError("manager secrets file must be a JSON object of string values")

    existing = secret_values.get(token_ref)
    if existing:
        try:
            request_json(base_url, "/api/v2/users/me", existing)
            print(f"Manager Coder token reference '{token_ref}' is already valid.")
            return 0
        except RuntimeError:
            print(f"Stored Manager token '{token_ref}' is invalid or expired; rotating it.")

    bootstrap_token = os.environ.get("CODER_SESSION_TOKEN") or legacy_env.get("CODER_SESSION_TOKEN", "")
    if bootstrap_token and not valid_bootstrap_token(base_url, bootstrap_token):
        bootstrap_token = ""
    if not bootstrap_token:
        candidate = cli_token(base_url)
        if candidate and valid_bootstrap_token(base_url, candidate):
            bootstrap_token = candidate
    if not bootstrap_token:
        raise RuntimeError(
            "no valid bootstrap credential found; run 'coder login <CODER_URL>' once, or set CODER_SESSION_TOKEN"
        )

    user = request_json(base_url, "/api/v2/users/me", bootstrap_token)
    user_id = user.get("id") if isinstance(user, dict) else None
    if not user_id:
        raise RuntimeError("Coder did not return the authenticated user id")

    token_name = f"openflows-manager-{int(time.time())}-{secrets.token_hex(3)}"
    token_result = request_json(
        base_url,
        f"/api/v2/users/{quote(str(user_id), safe='')}/keys/tokens",
        bootstrap_token,
        # Omit lifetime so Coder applies the deployment's configured default;
        # admins may cap token lifetimes below any requested value.
        {"token_name": token_name},
    )
    manager_token = token_result.get("key") if isinstance(token_result, dict) else None
    if not manager_token:
        raise RuntimeError("Coder did not return a new API token")

    secret_values[token_ref] = manager_token
    atomic_write_secrets(secrets_file, secret_values)
    print(f"Created and stored the Manager Coder API token as '{token_ref}'.")
    print(f"Secrets file permissions set to 0600: {secrets_file}")
    print("Restart the Manager to load the token and start its provisioning workers.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        print(f"bootstrap-manager-coder-token: {error}", file=sys.stderr)
        raise SystemExit(1)
