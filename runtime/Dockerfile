# OpenFlows workspace image: the runtime every role workspace starts from.
#
# It holds the tools the startup steps use, the OpenFlows programs, and the shared
# startup library (lib/bootstrap.sh). Role assets (personas, skills, hooks) are not
# part of it yet: workspaces still install them from the tenant artifacts volume or
# the Manager's bundle.
#
# The programs are copied from /usr/local/bin of the image named by PROGRAMS_IMAGE:
#   release  --build-arg PROGRAMS_IMAGE=ghcr.io/the-agenticflow/openflows@sha256:…
#   local    runtime/build-image.sh TAG DIR   (packs programs built from a checkout)
#
# Supported platform: linux/amd64, the platform the role templates run.
#
# This is a minimal runtime, not a development environment: it has no compilers,
# make or language toolchains. A local install whose repositories need them can
# extend it (FROM ghcr.io/the-agenticflow/openflows-workspace@sha256:…) and set
# TF_VAR_workspace_image; hosted workspaces always run the release's image.

# The default only exists to make a build without the argument fail with a
# readable error naming what is missing.
ARG PROGRAMS_IMAGE=set-the-programs-image-build-arg
FROM ${PROGRAMS_IMAGE} AS programs

FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55

ARG OPENFLOWS_VERSION=dev

# Ubuntu's image has a default `ubuntu` user with uid 1000. The workspace user
# takes that uid, which is also what existing workspace volumes are owned by.
#
# Passwordless sudo exists for startup only: it takes ownership of the
# workspace paths, reads the root-owned runtime secret and installs mounted
# development programs (runtime/lib/bootstrap.sh). A hosted workspace removes
# this rule before it reports readiness (of_release_privileges), so its agents
# run without root; each start restores it from the image. Local workspaces keep
# it so agents can install tools on the operator's own machine.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git jq openssh-client procps python3 sudo \
    && rm -rf /var/lib/apt/lists/* \
    && (userdel -r ubuntu 2>/dev/null || true) \
    && useradd -m -u 1000 -s /bin/bash coder \
    && printf 'coder ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/coder \
    && chmod 0440 /etc/sudoers.d/coder \
    && mkdir -p /home/coder/workspace \
    && chown -R coder:coder /home/coder

COPY --from=programs /usr/local/bin/openflows /usr/local/bin/openflows-harness /usr/local/bin/
COPY lib/bootstrap.sh /opt/openflows/runtime/lib/bootstrap.sh
COPY bin/openflows-credential-refresh /opt/openflows/runtime/bin/openflows-credential-refresh
COPY VERSION /opt/openflows/runtime/VERSION
RUN chmod 0755 /usr/local/bin/openflows /usr/local/bin/openflows-harness \
        /opt/openflows/runtime/bin/openflows-credential-refresh \
    && chmod 0644 /opt/openflows/runtime/lib/bootstrap.sh /opt/openflows/runtime/VERSION

LABEL org.opencontainers.image.title="openflows-workspace" \
      org.opencontainers.image.description="OpenFlows role workspace runtime" \
      org.opencontainers.image.version="${OPENFLOWS_VERSION}" \
      org.opencontainers.image.source="https://github.com/The-AgenticFlow/openflows" \
      org.opencontainers.image.licenses="MIT"

USER coder
WORKDIR /home/coder/workspace
