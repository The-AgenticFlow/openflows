#!/usr/bin/env python3
"""Local workspace GitHub credentials from the local Manager.

The rotating GitHub refresh token stays with Manager. Workspaces hold only a
short-lived user access token and a local broker key mounted read-only.
"""

import json
import os
import pathlib
import pwd
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

TOKEN_FILE = pathlib.Path("/home/coder/.config/openflows/github-api-token")
REPOSITORY_FILE = TOKEN_FILE.with_name("repository")
KEY_FILE = pathlib.Path("/run/openflows/local-broker-key")
MANAGER_URL = os.environ.get("OPENFLOWS_LOCAL_MANAGER_URL", "http://openflows-local-manager:3002").rstrip("/")
REPOSITORY = os.environ.get("OPENFLOWS_CONNECTED_LOCAL_REPOSITORY", "")


def repository_name():
    value = REPOSITORY.strip()
    if not value and REPOSITORY_FILE.exists():
        value = REPOSITORY_FILE.read_text().strip()
    if not value:
        value = subprocess.check_output(
            ["git", "-C", "/home/coder/.openflows/artifacts/repo", "remote", "get-url", "origin"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    value = re.sub(r"^(https://github\.com/|git@github\.com:)", "", value)
    value = value.removesuffix(".git").strip("/")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", value):
        raise RuntimeError("connected-local repository is not configured")
    return value


def fetch():
    repository = repository_name()
    key = subprocess.check_output(["sudo", "-n", "cat", str(KEY_FILE)], text=True).strip()
    request = urllib.request.Request(
        MANAGER_URL + "/api/v1/local/github-credential",
        data=json.dumps({"repository": repository}).encode(),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        token = json.load(response)["token"]
    if not token:
        raise RuntimeError("local Manager returned an empty GitHub token")
    TOKEN_FILE.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temp = tempfile.mkstemp(prefix="github-api-token.", dir=TOKEN_FILE.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write(token)
            stream.flush()
            os.fsync(stream.fileno())
        if os.geteuid() == 0:
            coder = pwd.getpwnam("coder")
            os.chown(temp, coder.pw_uid, coder.pw_gid)
        os.replace(temp, TOKEN_FILE)
    finally:
        if os.path.exists(temp):
            os.unlink(temp)


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "setup":
        fetch()
        REPOSITORY_FILE.write_text(repository_name() + "\n")
        REPOSITORY_FILE.chmod(0o600)
        subprocess.Popen(
            [sys.executable, __file__, "watch"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    elif command == "watch":
        while True:
            time.sleep(1800)
            try:
                fetch()
            except (OSError, KeyError, ValueError, RuntimeError, urllib.error.URLError):
                # A later request or Git operation will surface GitHub/Manager
                # denial. Never replace a working token with an error page.
                pass
    elif command == "credential":
        if len(sys.argv) > 2 and sys.argv[2] != "get":
            return
        fields = dict(line.strip().split("=", 1) for line in sys.stdin if "=" in line)
        if fields.get("protocol") != "https" or fields.get("host") != "github.com":
            return
        fetch()
        token = TOKEN_FILE.read_text().strip()
        if not token:
            raise RuntimeError("connected-local GitHub token is unavailable")
        print("username=x-access-token")
        print("password=" + token)
    elif command == "token":
        print(TOKEN_FILE.read_text().strip())
    else:
        raise RuntimeError("expected setup, watch, credential, or token")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        print("connected-local GitHub credentials unavailable; check the local Manager and GitHub authorization", file=sys.stderr)
        sys.exit(1)
