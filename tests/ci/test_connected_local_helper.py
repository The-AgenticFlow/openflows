"""Credential cache behavior when the local Manager rejects a workspace."""

import importlib.util
import pathlib
import tempfile
import unittest
import urllib.error
from unittest.mock import patch


HELPER = pathlib.Path(__file__).resolve().parents[2] / "scripts/connected-local-github.py"


class ConnectedLocalHelperTest(unittest.TestCase):
    def test_explicit_denial_removes_cached_token_but_outage_retains_it(self):
        spec = importlib.util.spec_from_file_location("connected_local_github", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        with tempfile.TemporaryDirectory() as directory:
            helper.TOKEN_FILE = pathlib.Path(directory) / "token"
            helper.REPOSITORY = "alice/private"
            helper.TOKEN_FILE.write_text("previous-token")
            with patch.object(helper.subprocess, "check_output", return_value="broker-key"):
                with patch.object(helper.urllib.request, "urlopen", side_effect=urllib.error.URLError("offline")):
                    with self.assertRaises(urllib.error.URLError):
                        helper.fetch()
                self.assertEqual(helper.TOKEN_FILE.read_text(), "previous-token")
                with patch.object(helper.urllib.request, "urlopen", side_effect=urllib.error.HTTPError("broker", 404, "denied", {}, None)):
                    with self.assertRaises(urllib.error.HTTPError):
                        helper.fetch()
                self.assertFalse(helper.TOKEN_FILE.exists())


if __name__ == "__main__":
    unittest.main()
