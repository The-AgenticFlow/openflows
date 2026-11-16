#!/usr/bin/env bash
# Check that every role template runs its hosted workspace image from REPOSITORY.
#
#   scripts/check-template-image-repository.sh TEMPLATES_DIR REPOSITORY
#
# Hosted workspaces start from "<workspace_image_repository>@<digest>": the
# repository is the default of the template variable workspace_image_repository,
# and only the digest comes from the Manager. A release that pins the digest of
# another repository (a fork or a mirror) would produce workspaces that cannot
# pull their image, so template-release.yml runs this before pinning.
set -euo pipefail

templates="${1:?usage: $0 TEMPLATES_DIR REPOSITORY}"
expected="${2:?usage: $0 TEMPLATES_DIR REPOSITORY}"
failed=0
found=0
for template in "$templates"/openflows-*/main.tf; do
    [ -f "$template" ] || continue
    found=$((found + 1))
    actual="$(awk '
        /^variable "workspace_image_repository"/ { inside = 1 }
        inside && /default[[:space:]]*=/ { sub(/.*default[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit }
        inside && /^}/ { exit }
    ' "$template")"
    if [ "$actual" != "$expected" ]; then
        echo "::error file=${template}::workspace_image_repository defaults to '${actual:-<none>}', but the release pins the digest of ${expected}" >&2
        failed=1
    fi
done
if [ "$found" -eq 0 ]; then
    echo "::error::no role templates found in ${templates}" >&2
    exit 1
fi
if [ "$failed" -ne 0 ]; then
    echo "Point workspace_image_repository at ${expected} in every template, or publish the workspace image where the templates expect it." >&2
    exit 1
fi
echo "All ${found} templates run their hosted workspace image from ${expected}"
