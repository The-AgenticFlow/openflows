#!/bin/sh
# The oracle lives outside the candidate repository so FORGE cannot change it.
set -eu
printf 'CHECKOUT_CWD=%s\n' "$(pwd -P)"
test "$(cat answer.txt)" = 42
printf 'ACCEPTANCE_OK\n'
