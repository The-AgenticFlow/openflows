"""Prepare curl authentication from the workspace brokered credential."""
import os
from pathlib import Path

token = Path(os.environ['OPENFLOWS_GITHUB_API_TOKEN_FILE']).read_text().strip()
assert token, 'No brokered GitHub credential'
header = Path.home() / '.config' / 'openflows-e2e' / 'github-auth-header'
header.parent.mkdir(parents=True, exist_ok=True)
header.write_text('Authorization: Bearer ' + token + '\n')
header.chmod(0o600)
print('Prepared curl authentication for the workspace owner.')
