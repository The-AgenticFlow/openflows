# Token guide

Local OpenFlows uses two distinct credentials:

1. **GitHub App user authorization.** At <http://localhost:3002>, install the
   shared OpenFlows App on a repository and choose **Connect GitHub**. The local
   Manager obtains and refreshes a user access token through GitHub's device
   flow. Workspaces receive the token through the Manager broker. Access is
   limited to repositories and permissions shared by the user and App.
2. **Coder session token.** Create one at
   <http://localhost:7080/settings/tokens> for workspace provisioning and
   chat creation. Setup helps create it and saves it in the private `.env`.

The Manager keeps GitHub refresh credentials under `~/.config/openflows/`.
Workspace token files are short lived and replaced by the local credential
helper. Never print these tokens or commit `.env`, `.local/`, or local credential
files.

If GitHub access stops working, check the App installation and reconnect in
the local console. If a Coder token expires, create a new one and rerun setup.
See the [setup reference](docs/setup-reference.md) for diagnostics.
