"""Prepare curl authentication from the workspace's real inherited credential, without printing it."""
from pathlib import Path
from urllib.parse import urlsplit

credential = urlsplit((Path.home() / '.git-credentials').read_text().splitlines()[0])
assert credential.password, 'No inherited GitHub credential'
header = Path.home() / '.config' / 'openflows-e2e' / 'github-auth-header'
header.parent.mkdir(parents=True, exist_ok=True)
header.write_text('Authorization: Bearer ' + credential.password + '\n')
header.chmod(0o600)
print('Prepared curl authentication for the inherited workspace owner.')
