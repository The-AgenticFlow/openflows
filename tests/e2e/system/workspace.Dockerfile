# Workspace image for the system E2E: the OpenFlows workspace image built from this
# checkout (runtime/Dockerfile holding the candidate programs) plus the E2E helpers.
# bootstrap.sh builds RUNTIME_IMAGE with runtime/build-image.sh.

ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
COPY prepare_http.py acceptance.sh /opt/openflows-e2e/
ENV OPENFLOWS_HOOK_LOGS=true
