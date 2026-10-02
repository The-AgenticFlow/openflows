# Verification checkout

SENTINEL submits a verification command through A2A. FORGE executes it in a
temporary detached checkout of the candidate commit, using the tools already
installed in its workspace. No additional image, Docker engine, or image variable
is needed.

FORGE installs and configures the project's tools during building. Every delegated
command starts a fresh Bash login shell, which loads the user's current login
profile rather than relying only on the executor daemon's old PATH. Tool activation
must be persisted in that profile or performed by a project script; exports in an
unrelated one-off shell are not shared. After profile loading, the runner returns
to the temporary checkout, removes Git overrides and passes the original argv
literally to `exec`. No language-specific installation or readiness phase is added.

SENTINEL receives argv, tested HEAD, exit code, stdout and stderr. A missing
executable produces shell exit 127 with an EXECUTOR_SETUP diagnostic; a failure to
create the checkout or start the shell has no command exit code. Failed commands,
timeouts and repairable setup failures are returned to FORGE with the normal
testing rejection, which enters building and keeps the approved plan. FORGE fixes
the code/environment and returns to testing. Blocked is reserved for external
prerequisites it cannot resolve; no extra repair state is needed.

Deploy the updated NEXUS relay and FORGE harness together. Executors advertise
`checkout_version: 3` when claiming tasks; the relay rejects older executors so
verification cannot silently fall back to the former execution mode.
Freshly provisioned SENTINEL and FORGE workspaces also receive `A2A_PAIR_TOKEN`.
The relay requires that token for pair-scoped verification RPCs, so older
workspaces without it must be reprovisioned before they can submit or claim
verification tasks.

The executor requires a clean working checkout and captures its HEAD. Independent
local clones use separate Git objects and indexes, preventing ordinary test Git
commands from changing FORGE's branch. Ignored and untracked files are not copied;
project dependencies kept only in the working directory must be installed in the
temporary checkout. HOME, PATH, package caches, and network access remain available.

Commands run in their own process group with the existing timeout and cancellation
handling. Relative artifacts are read from the temporary checkout before it is
removed; traversal and links outside the checkout are rejected. A baseline copy
detects committed-source edits, deletions, permission changes, and changed symlinks.
Generated files are allowed. Verification evidence requires both unchanged source
and the original FORGE checkout still being clean at the captured HEAD.

This is not process isolation: verification has the permissions and environment of
the FORGE executor. Existing command-policy checks remain diagnostic guards. Strong
container isolation is outside the scope of this execution mode.
