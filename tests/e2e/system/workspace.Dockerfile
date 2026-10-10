# Builds the workspace image used by the production-bootstrap E2E scenario.
# Starts from the same pinned Ubuntu release as the other container tests.
# Includes real Git, curl, Python, and process tools required during startup.
# Creates the coder account and its normal workspace directory.
# Allows the production startup script to install the candidate binaries.
# The runner mounts binaries compiled from the pull request as read-only files.
# Coder and Terraform start this image using the bundled production template.
# This file provides the image; it does not replace the template startup script.
# Python supports existing startup scripts and the disposable test fixtures.
# The controller and harness running inside the workspace remain Rust binaries.
# The image contains no production account tokens or model-provider API keys.

FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git openssh-client procps python3 sudo \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash coder \
    && printf 'coder ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/coder \
    && chmod 0440 /etc/sudoers.d/coder \
    && mkdir -p /home/coder/workspace \
    && chown -R coder:coder /home/coder
USER coder
WORKDIR /home/coder/workspace
