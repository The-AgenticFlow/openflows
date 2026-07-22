#!/bin/bash
# Build Terraform template archives for all 5 roles.
# Run from the repo root: bash rebuild_templates.sh
set -e

TEMPLATE_DIR="$(cd "$(dirname "$0")" && pwd)"
ROLES=("forge" "sentinel" "nexus" "vessel" "lore")

for role in "${ROLES[@]}"; do
    src_dir="$TEMPLATE_DIR/openflows-$role"
    if [ -d "$src_dir" ]; then
        tar -czf "$TEMPLATE_DIR/openflows-$role.tar.gz" -C "$src_dir" .
        echo "  $role: created openflows-$role.tar.gz"
    else
        echo "  $role: SKIPPED (openflows-$role/ not found)"
    fi
done

echo "Template archives rebuilt."
