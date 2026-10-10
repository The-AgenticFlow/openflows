FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git openssh-client procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash coder \
    && mkdir -p /home/coder/workspace \
    && chown -R coder:coder /home/coder
COPY openflows-harness /usr/local/bin/openflows-harness
COPY ci_a2a_relay /usr/local/bin/ci-a2a-relay
USER coder
WORKDIR /home/coder/workspace
