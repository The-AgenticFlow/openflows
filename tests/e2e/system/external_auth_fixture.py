# Implements a small disposable OAuth device-flow provider for container E2E.
# Coder starts device authorization through this provider's HTTP interface.
# A device receives a short-lived code and waits for explicit operator approval.
# Unapproved devices cannot exchange their code for an access token.
# Only the disposable operator credential can approve a pending device.
# Expired or already exchanged device codes cannot be reused.
# The issued token identifies a disposable account rather than a real user.
# The user endpoint lets Coder validate that account through its normal API.
# Coder stores the resulting link and injects its token through Terraform.
# This exercises Coder linking and token injection without production secrets.
# It does not certify that GitHub's real OAuth service or App installation works.

"""Local device-flow provider for linking a disposable account through Coder's API.

This exercises Coder's linking and Terraform token injection, not GitHub OAuth.
"""
import secrets
import time
from urllib.parse import parse_qs, urlsplit


class ExternalAuth:
    def __init__(self, reject):
        self.reject = reject
        self.devices = {}

    def handle(self, method, url, body, token):
        parsed = urlsplit(url)
        query = parse_qs(parsed.query)
        if method == 'GET' and parsed.path == '/user':
            if token != 'ci-vessel-token':
                raise self.reject(401, 'Invalid disposable OAuth token')
            return 200, {'login': 'vessel', 'id': 3, 'name': 'Disposable E2E account'}
        if method == 'POST' and parsed.path == '/approve':
            if token != 'ci-operator-token':
                raise self.reject(403, 'Device approval requires the disposable operator')
            device = next((d for d in self.devices.values()
                if d['user_code'] == body.get('user_code') and d['expires'] > time.monotonic()), None)
            if not device:
                raise self.reject(404, 'No pending device')
            device['approved'] = True
            return 200, {'approved': True}
        if method != 'POST' or query.get('client_id') != ['ci-coder']:
            raise self.reject(404, 'Unsupported disposable OAuth request')
        if parsed.path == '/device/code':
            code, user_code = secrets.token_hex(16), secrets.token_hex(4)
            self.devices[code] = {'user_code': user_code, 'approved': False,
                'expires': time.monotonic() + 180}
            return 200, {'device_code': code, 'user_code': user_code,
                'verification_uri': 'http://oauth:8080/approve', 'expires_in': 180, 'interval': 1}
        if parsed.path == '/oauth/token':
            if query.get('grant_type') != ['urn:ietf:params:oauth:grant-type:device_code']:
                raise self.reject(400, 'Unsupported grant')
            code = query.get('device_code', [''])[0]
            device = self.devices.get(code)
            if not device or device['expires'] <= time.monotonic():
                return 400, {'error': 'expired_token'}
            if not device['approved']:
                return 400, {'error': 'authorization_pending'}
            del self.devices[code]
            return 200, {'access_token': 'ci-vessel-token', 'token_type': 'Bearer',
                'scope': 'repo', 'expires_in': 3600}
        raise self.reject(404, 'Unsupported disposable OAuth endpoint')
