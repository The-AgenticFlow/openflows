# Tests the disposable OAuth provider through its actual HTTP interface.
# Runs a local fixture server in a fresh temporary directory for each test.
# Starts a device authorization request using the configured disposable client.
# Checks that token exchange is blocked while human approval is pending.
# Checks that an unrelated credential cannot approve the device.
# Approves the pending device using the explicit disposable operator credential.
# Confirms that an approved device can obtain the expected disposable token.
# Confirms that the same device code cannot be exchanged a second time.
# Uses only Python's standard library and the existing fixture test helpers.
# Temporary servers and files are cleaned up by the shared test fixture lifecycle.
# This verifies the local contract; the production-bootstrap test exercises Coder.

"""The disposable OAuth provider must require an explicit human device approval."""
import unittest
from urllib.parse import urlencode

import json
import test_fixtures


class ExternalAuthTest(test_fixtures.FixtureCase):
    def request(self, path, body, token="ci-model-key"):
        status, raw, _ = test_fixtures.FixtureTest.request(self, path, body, token)
        return status, json.loads(raw)

    def setUp(self):
        self.start('oauth')

    def test_only_approved_device_can_link_a_coder_account(self):
        status, device = self.request('/device/code?client_id=ci-coder', {})
        self.assertEqual(status, 200)
        query = urlencode({'client_id': 'ci-coder', 'device_code': device['device_code'],
            'grant_type': 'urn:ietf:params:oauth:grant-type:device_code'})
        status, response = self.request('/oauth/token?' + query, {})
        self.assertEqual(status, 400)
        self.assertEqual(response['error'], 'authorization_pending')
        self.assertEqual(self.request('/approve', {'user_code': device['user_code']})[0], 403)
        self.assertEqual(self.request('/approve', {'user_code': device['user_code']}, 'ci-operator-token')[0], 200)
        status, response = self.request('/oauth/token?' + query, {})
        self.assertEqual(status, 200)
        self.assertEqual(response['access_token'], 'ci-vessel-token')
        self.assertEqual(self.request('/oauth/token?' + query, {})[0], 400)


if __name__ == '__main__':
    unittest.main()
