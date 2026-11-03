# Workspace image for the real production templates and candidate Rust binaries.

FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git jq openssh-client procps python3 sudo \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash coder \
    && printf 'coder ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/coder \
    && chmod 0440 /etc/sudoers.d/coder \
    && mkdir -p /home/coder/workspace \
    && chown -R coder:coder /home/coder
USER coder
WORKDIR /home/coder/workspace
COPY prepare_http.py acceptance.sh /opt/openflows-e2e/
ENV OPENFLOWS_HOOK_LOGS=true
