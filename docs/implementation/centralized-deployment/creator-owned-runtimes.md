# GitHub sign-in and creator-owned runtimes

OpenFlows and Coder retain independent GitHub sign-in sessions. A signed-in
OpenFlows user explicitly authorizes the OpenFlows OAuth client in Coder.
OpenFlows verifies Coder's authenticated `/api/v2/users/me` response and requires
an active, non-service-account user with `login_type=github`. This proves control
of both accounts; it does **not** claim that their underlying GitHub numeric IDs
match (Coder's standard user API does not expose that subject).

Links are one-to-one within the configured Coder deployment and cannot be
silently replaced. The tenant creator is persisted at creation; retries do not
change ownership. Provisioning adds basic Coder organization membership, never
administrator roles. Deleting a user-owned tenant does not delete its Coder user.

## Operator setup

For a new installation, use the consolidated
[setup, doctor, and serve workflow](operator-deployment.md). It creates the
Coder OAuth application and stores its ID/secret automatically. The manual
steps below remain a reference for externally managed OAuth registrations.

1. Keep the existing OpenFlows GitHub application and callback unchanged.
2. Create a **separate GitHub OAuth App for Coder sign-in**, using:
   - Homepage: your browser-accessible Coder URL.
   - User authorization callback: `<CODER_ACCESS_URL>` (Coder handles the GitHub callback internally).
   This is distinct from Coder's external-auth integration for Git operations.
3. Set these in Compose's `.env` (not the Manager's `.env.prod`):

   ```dotenv
   CODER_ACCESS_URL=https://your-coder-host
   CODER_OAUTH2_GITHUB_CLIENT_ID=<GitHub OAuth App client ID>
   CODER_OAUTH2_GITHUB_CLIENT_SECRET=<GitHub OAuth App secret>
   CODER_OAUTH2_GITHUB_ALLOWED_ORGS=<your GitHub organization>
   CODER_OAUTH2_GITHUB_ALLOW_EVERYONE=false
   CODER_OAUTH2_PROVIDER_ENABLE=true
   ```

   For intentionally public SaaS signups, use an empty allowed-org list and
   explicitly enable `CODER_OAUTH2_GITHUB_ALLOW_EVERYONE=true`. Do not enable it
   accidentally on a restricted deployment.
4. Recreate only the Coder server with `docker compose up -d coder`.
   Keep existing volumes. Do not disable the existing administrator login until
   you have verified GitHub login and administrative access.
5. In Coder Deployment Settings → OAuth2 Applications, register **OpenFlows**:
   - Callback: `<OPENFLOWS_PUBLIC_URL>/auth/coder/callback`.
   - Allowed scope: `user:read_personal`.
   - Generate a confidential client secret.
6. Store that secret in a mode-0600 file under the configured filesystem secret
   directory. Do not paste it in chat or commit it. Set in `.env.prod`:

   ```dotenv
   CODER_URL=https://your-coder-host
   OPENFLOWS_CODER_OAUTH_CLIENT_ID=<Coder OAuth application ID>
   OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF=/absolute/path/to/openflows-secrets/coder-oauth-client-secret
   ```

   `CODER_URL` must reach the same deployment from both the browser and Manager.
   HTTP is accepted only for explicit localhost/loopback development.
   The client ID/secret here are issued by **Coder**, not GitHub.
7. Restart the Manager: `cargo run -p openflows-manager`. Migrations run at
   startup. No template release is needed for this ownership change.

## User testing

1. Sign into OpenFlows with GitHub.
2. Click **Connect my GitHub-backed Coder account** in the test console.
3. Sign into Coder using GitHub and approve OpenFlows' request.
4. After the redirect, click **Check Coder account** and confirm the account ID.
5. Create a new tenant. Its workspaces should be owned by that Coder user.
6. Check that retries retain that owner, and another tenant created by the same
   user can use the same account.

No manual Coder API token is required from the user. The verified Coder user ID
and deployment form the durable identity; the renewable OAuth refresh credential
is stored separately in the configured secret provider. Operators must protect
and revoke it when the account is disconnected.

## Existing tenants

Existing tenants deliberately retain a null creator until an explicit migration.
Provisioning reports `OWNER_MIGRATION_REQUIRED`; it does not infer ownership
from an organization administrator or the actor pressing Retry. Do not update
only the database owner: existing Coder workspaces must also be migrated or
explicitly recreated after assessing data retention. Nothing in this change
automatically deletes or transfers existing workspaces.
