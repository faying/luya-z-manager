#!/bin/sh
# Builds lanid's OUI table (docs/designs/lan-client-id.md §3.3, §4a) from an IEEE oui.txt that
# you supply; nothing is downloaded. The public repository carries this script, not the table.
#
#   scripts/lanid/make-oui.sh oui.txt [out.tsv]     (default out: ./oui.tsv)
#
# Output: one `aabbcc<TAB>Vendor` per line (vendor cut to 64 bytes), sorted, what luyaz-core
# reads from /data/luyaz/lanid/oui.tsv (src/lanid/classify.rs `Oui`). Only the
# `XX-XX-XX   (hex)   Vendor` lines are used; locally administered prefixes are skipped
# (lanid never looks those up).
set -eu
IN=${1:?usage: make-oui.sh oui.txt [out.tsv]}
OUT=${2:-oui.tsv}
LC_ALL=C awk '
    /^[0-9A-Fa-f][0-9A-Fa-f]-[0-9A-Fa-f][0-9A-Fa-f]-[0-9A-Fa-f][0-9A-Fa-f][ \t]+\(hex\)/ {
        p = tolower(substr($1, 1, 2) substr($1, 4, 2) substr($1, 7, 2))
        # second hex digit with bit 1 set: locally administered
        if (index("2367abef", substr(p, 2, 1)) > 0) next
        v = $0
        sub(/^[^)]*\)[ \t]*/, "", v)
        gsub(/[\001-\037\177]/, "", v)
        sub(/[ \t]+$/, "", v)
        if (v == "") next
        print p "\t" substr(v, 1, 64)
    }' "$IN" | sort -u -k1,1 > "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
echo "make-oui.sh: $(wc -l < "$OUT" | tr -d ' ') prefixes in $OUT"
