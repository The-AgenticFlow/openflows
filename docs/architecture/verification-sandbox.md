# Verification checkout

SENTINEL submits a verification command through A2A. FORGE executes it in a
temporary detached checkout of the candidate commit, using the tools already
installed in its workspace. No additional image, Docker engine, or image variable
is needed.

Deploy the updated NEXUS relay and FORGE harness together. Executors advertise
`checkout_version: 1` when claiming tasks; the relay rejects older executors so
verification cannot silently fall back to the former execution mode.

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
