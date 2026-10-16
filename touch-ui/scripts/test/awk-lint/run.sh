#!/bin/sh
# The device's busybox awk (OpenWrt build of 1.36.1) parses
#     x = cond ? a : b        and        x = (cond) ? a : b
# as x = cond — the ?: is dropped silently. Upstream busybox (the test
# container) gets it right, so the behaviour tests cannot catch it. This
# lint flags any "=" whose right-hand side reaches a "?" outside parentheses;
# write x = (cond ? a : b) instead.
# SPDX-License-Identifier: MIT
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0; FAIL=0

lint() {
    awk '
        /^[ \t]*#/ { next }
        {
            s = $0; n = length(s); hit = 0
            for (i = 2; i < n && !hit; i++) {
                ch = substr(s, i, 1)
                if (ch != "=") continue
                pv = substr(s, i - 1, 1); nx = substr(s, i + 1, 1)
                if (pv ~ /[=!<>~]/ || nx ~ /[=~]/) continue
                depth = 0; q = 0
                for (j = i + 1; j <= n; j++) {
                    c = substr(s, j, 1)
                    if (c == "\"") { q = !q; continue }
                    if (q) continue
                    if (c == "(") depth++
                    else if (c == ")") { if (--depth < 0) break }
                    else if (c == ";" || c == "}" || c == "{" || c == "#" || c == "'\''") { if (depth <= 0) break }
                    else if (c == "?" && depth == 0 && substr(s, j - 1, 1) != "$") { hit = 1; break }   # $? is shell
                }
            }
            if (hit) printf "%s:%d: %s\n", FILENAME, FNR, $0
        }' "$@"
}

# the lint itself must see the known-bad forms and pass the fixed ones
T=$(mktemp -d)
printf '%s\n' '    m = (c %% 2) ? a : b' '    x[i] = d < 0 ? -d : d' >"$T/bad.sh"
printf '%s\n' '    m = ((c %% 2) ? a : b)' '    x[i] = (d < 0 ? -d : d)' '    if (a == b) print (s > 0 ? 1 : 0)' \
    '    # y = a ? b : c' '    exit (h >= 1) ? 1 : 0' '    u="http://x/?a=b"' '    RC=$?' >"$T/good.sh"
if [ "$(lint "$T/bad.sh" | wc -l)" -eq 2 ]; then PASS=$((PASS + 1)); echo "  ok   lint flags both bad forms"
else FAIL=$((FAIL + 1)); echo "  FAIL lint missed a bad form"; fi
if [ -z "$(lint "$T/good.sh")" ]; then PASS=$((PASS + 1)); echo "  ok   lint passes parenthesised forms"
else FAIL=$((FAIL + 1)); echo "  FAIL lint flags a good form:"; lint "$T/good.sh"; fi
rm -rf "$T"

for f in "$SCRIPTS"/*.sh; do
    out=$(lint "$f")
    if [ -z "$out" ]; then PASS=$((PASS + 1)); echo "  ok   $(basename "$f")"
    else FAIL=$((FAIL + 1)); echo "  FAIL $(basename "$f"): unparenthesised ?: after =";  echo "$out"; fi
done

echo "awk-lint: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
