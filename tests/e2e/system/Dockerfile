FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
# Ubuntu mirrors sync asynchronously, so an index can arrive with an unexpected
# size mid-sync. Retry instead of failing a whole CI job on that race.
RUN set -eux; \
    for attempt in 1 2 3; do \
        apt-get -o Acquire::Retries=3 update \
        && apt-get -o Acquire::Retries=3 install -y --no-install-recommends python3 git \
        && break; \
        if [ "$attempt" = 3 ]; then exit 1; fi; \
        sleep 15; \
    done; \
    rm -rf /var/lib/apt/lists/*
WORKDIR /opt/fixtures
COPY . .
ENV PYTHONDONTWRITEBYTECODE=1
CMD ["python3", "-m", "unittest", "discover", "-s", ".", "-v"]
