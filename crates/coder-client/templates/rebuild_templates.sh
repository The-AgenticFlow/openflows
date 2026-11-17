#!/bin/bash
# Build Terraform template archives for all 5 roles, for each backend variant
# that exists in this checkout.
# Run from the repo root: bash rebuild_templates.sh
set -e

TEMPLATE_DIR="$(cd "$(dirname "$0")" && pwd)"
ROLES=("forge" "sentinel" "nexus" "vessel" "lore")

# Docker is the flat layout; Kubernetes lives under kubernetes/ with a prefixed
# archive name, which is what the Manager looks for in a release bundle.
pack() {
    local src_dir="$1" archive="$2" label="$3"
    if [ -d "$src_dir" ]; then
        tar -czf "$TEMPLATE_DIR/$archive" \
            --exclude=.terraform --exclude=.terraform.lock.hcl \
            -C "$src_dir" .
        echo "  $label: created $archive"
    else
        echo "  $label: SKIPPED ($src_dir not found)"
    fi
}

for role in "${ROLES[@]}"; do
    pack "$TEMPLATE_DIR/openflows-$role" "openflows-$role.tar.gz" "$role"
done

for role in "${ROLES[@]}"; do
    pack "$TEMPLATE_DIR/kubernetes/openflows-$role" \
        "openflows-kubernetes-$role.tar.gz" "kubernetes/$role"
done

echo "Template archives rebuilt."
