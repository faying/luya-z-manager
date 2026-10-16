#!/bin/sh
# rs/devui/lang/en.tsv → rs/devui/lang/en.inc (the screen's English rows; rs/devui/build.rs).
#
#   sh scripts/lang-gen.sh rs/devui/lang/en.tsv > rs/devui/lang/en.inc
#
# en.tsv: one line per text, <中文><TAB><English>; # starts a comment line,
# blank lines are skipped. Both columns are written exactly as they would sit
# between the quotes of a C string (\n, \", %% stay as typed). Key 场景|中文 =
# TRC; English one|other = TRN. build.rs sorts, so order is free.
# Anything else (no tab, two tabs, empty column) stops the build.
# SPDX-License-Identifier: MIT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 en.tsv" >&2; exit 2; }
awk -F '\t' '
/^#/ || /^[ \t]*$/ { next }
{
    if (NF != 2 || $1 == "" || $2 == "") { printf "%s:%d: want <中文><TAB><English>\n", FILENAME, NR > "/dev/stderr"; bad = 1; next }
    if ($1 in seen) { printf "%s:%d: duplicate key (line %d)\n", FILENAME, NR, seen[$1] > "/dev/stderr"; bad = 1; next }
    seen[$1] = NR
    n = split($2, p, "|")
    if (n == 1)      printf "    { \"%s\", \"%s\", NULL },\n", $1, $2
    else if (n == 2) printf "    { \"%s\", \"%s\", \"%s\" },\n", $1, p[1], p[2]
    else { printf "%s:%d: English has more than one |\n", FILENAME, NR > "/dev/stderr"; bad = 1 }
}
END { exit bad }' "$1"
