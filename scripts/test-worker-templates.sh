#!/bin/bash
# Mocked plans only: no Docker/Coder infrastructure is created.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
for role in forge sentinel; do
    mkdir -p "$test_dir/$role/tests"
    cp "$root/crates/coder-client/templates/openflows-$role/main.tf" "$test_dir/$role/"
    cp "$root/tests/worker/runtime.tftest.hcl" "$test_dir/$role/tests/"
    terraform -chdir="$test_dir/$role" init -backend=false -input=false
    terraform -chdir="$test_dir/$role" validate
    terraform -chdir="$test_dir/$role" test
done
