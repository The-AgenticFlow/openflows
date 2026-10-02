# Verification checkout

SENTINEL submits a verification command through A2A. FORGE executes it in a
temporary detached checkout of the candidate commit, using the tools already
installed in its workspace. No additional image, Docker engine, or image variable
is needed.

FORGE startup preserves a working Rust toolchain or installs stable with rustup,
and installs the native build
prerequisites (`build-essential`, `pkg-config`, `libssl-dev`) before starting the
executor. It explicitly exports `$HOME/.cargo/bin` on PATH: a daemon does not read
interactive shell profiles, and installing Cargo after it starts does not update
its environment. Existing workspaces need the updated template and a restart, or
the same toolchain installation followed by an executor restart. Startup needs
access to Ubuntu package repositories, sh.rustup.rs, and static.rust-lang.org.

Deploy the updated NEXUS relay and FORGE harness together. Executors advertise
`checkout_version: 1` when claiming tasks; the relay rejects older executors so
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
