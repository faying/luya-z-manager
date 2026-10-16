#!/bin/sh
# The screen's English table (rs/devui/lang/en.tsv → en.inc, rs/devui/build.rs):
#   1. rs/devui/lang/en.inc is what scripts/lang-gen.sh makes from en.tsv now
#   2. | in the English at most once (one|other)
#   3. (info) rows whose English is longer in bytes than the Chinese
# The C UI's checks (every TR literal in src/ has a row, tests/lang_test.c) left with it in
# Phase 10 X5; the Rust table's own rules are rs/devui/src/lang.rs's tests.
# Host script (docker). SPDX-License-Identifier: MIT
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
exec docker run --rm -v "$ROOT":/src:ro -w /src rust:1.99.0-slim@sha256:24e632c09342c20abf8312cf4f61430a911c01ed3a5e4c02b87292b1c39c5273 sh -c '
rc=0
T=rs/devui/lang/en.tsv
sh scripts/lang-gen.sh $T > /tmp/inc || rc=1
if cmp -s /tmp/inc rs/devui/lang/en.inc; then echo "  ok   rs/devui/lang/en.inc is current"
else echo "  FAIL rs/devui/lang/en.inc is stale: sh scripts/lang-gen.sh $T > rs/devui/lang/en.inc"; rc=1; fi
awk -F "\t" "!/^#/ && NF == 2 && split(\$2, p, \"|\") > 2 { print \$1 \"  →  \" \$2 }" $T > /tmp/pipe2
if [ ! -s /tmp/pipe2 ]; then echo "  ok   | in the English at most once"
else echo "  FAIL more than one | in the English:"; sed "s/^/         /" /tmp/pipe2; rc=1; fi
echo "  info English longer than the Chinese (bytes):"
awk -F "\t" "!/^#/ && NF == 2 && length(\$2) > length(\$1) { printf \"         %3d > %3d  %s\n\", length(\$2), length(\$1), \$2 }" $T | sort -rn | head -15
exit $rc'
