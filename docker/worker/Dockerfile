FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55

# Linux amd64 matches the existing Coder agent / harness templates.
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl git jq python3 sudo unzip zip bash util-linux iptables docker.io \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash coder \
    && usermod -aG docker coder \
    && echo 'coder ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/coder \
    && chmod 0440 /etc/sudoers.d/coder

COPY docker/worker/entrypoint.sh /usr/local/bin/openflows-worker-entrypoint
RUN chmod 0755 /usr/local/bin/openflows-worker-entrypoint
ENV HOME=/home/coder DOCKER_HOST=unix:///var/run/docker.sock TESTCONTAINERS_HOST_OVERRIDE=localhost
WORKDIR /home/coder
ENTRYPOINT ["/usr/local/bin/openflows-worker-entrypoint"]
