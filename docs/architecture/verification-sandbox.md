# Verification checkout

SENTINEL submits a verification command through A2A. FORGE executes it in a
temporary detached checkout of the candidate commit, using the tools already
installed in its workspace. No additional image, Docker engine, or image variable
is needed.

The workspace template is language-neutral. During building, FORGE follows the
repository's setup instructions to install its tools, pinned versions, native
libraries and services. From that configured shell, after committing, it runs
`openflows-harness verify prepare -- <project readiness command>`. The command
must exercise the project's prerequisites in a fresh checkout and finish within
60 seconds. For expensive dependency installation, populate caches first and use
a short readiness probe. Readiness is not acceptance-test evidence.

Only a successful, source-preserving probe publishes an environment handoff in
the workspace Git directory (`openflows-verification.json`, mode 0600). It binds
the shell's exported variables and probe command to the candidate HEAD and current
lifecycle version. A repair needs fresh preparation after the pause. Testing
requires that handoff; every verification task reloads it, so changes to PATH,
virtual environments and tool managers do not require restarting the daemon.
Failed preparation removes previous readiness. Changing HEAD requires preparing
again. Do not print or commit the handoff: it can contain credentials.

This works for Go, Rust, Python, Node and mixed projects without language detection
in the harness. Shell aliases/functions are not exported executables: use a real
program or an explicit project wrapper. For checkout-local dependencies such as
`node_modules`, use a committed project wrapper that installs/restores dependencies
in its current checkout before running the requested test. A successful probe's
temporary checkout is removed; its generated dependencies are not carried into
later tasks. Both readiness and SENTINEL verification must use the same documented
project setup/wrapper contract.

`verify repair --reason "<command, error, task ID>"` requests an infrastructure pause
from testing. It preserves the HEAD and plan, invalidates test
evidence, and lets NEXUS wake FORGE to repair the environment without source edits.
FORGE prepares again and sets testing at that same clean HEAD, opening a new review
round. At most two resumptions are allowed per build; unresolved prerequisites or
exhausted retries remain blocked for operator action. Legacy blocked records that
already lost their HEAD cannot safely resume this way and still require replanning.
Ordinary `status set blocked` does not opt into automatic environment repair.

Deploy the updated NEXUS relay and FORGE harness together. Executors advertise
`checkout_version: 2` when claiming tasks; the relay rejects older executors so
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
