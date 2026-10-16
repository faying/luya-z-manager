#!/bin/sh
# Pixel parity of the Rust UI against the C render test (manager docs/designs/u60-touch-rewrite.md §4;
# the C UI was deleted in Phase 10 X5: MODEL_DIR/REF_DIR come from a checkout of tag legacy-c-touch-ui):
# replay C's runs on the Rust screen (rs/devui/src/render/mod.rs) and compare every shot's pixels
# (exact: C's pixel hash from the run log) and the text/glyph/geometry checks; for shots that
# differ, rs/pxcmp.py says how many pixels and where.
#
#   rs/parity.sh MODEL_DIR REF_DIR OUT_DIR [--pages home,cellular,…] [--lang zh|en|both]
#                [--theme light|dark|both] [--keep-going] [scene…]
#
# MODEL_DIR: scripts/test/render/render.sh --model DIR; REF_DIR: render.sh --png DIR (same run).
# Pages are the C shot names without .k (home, cellular, sms, power-menu, …; default: every page
# the runs shoot). No scene = every scene in MODEL_DIR. Needs `rs/build.sh native` first. Fonts as render.sh:
# LUYAZ_DEVICE_FONTS (ZTEZhengYuan.ttf, Roboto.ttf) and LUYAZ_NUNITO_DIR (Nunito-600/700/800.ttf).
# Writes OUT_DIR/parity.log (every line of every run) and the Rust PNGs of the shots that differ;
# prints the differing shots, the checks that differ and one summary line per run.
# Exit 1 when anything differs, when a run stopped at a step the Rust screen does not have yet
# (STOP; NOTREACHED for the shots it never got to), when a run crashed, or when a page asked
# for was not compared in any run.
# SPDX-License-Identifier: MIT
set -eu
[ $# -ge 3 ] || { sed -n 2,18p "$0"; exit 2; }
models=$(cd "$1" && pwd); ref=$(cd "$2" && pwd); mkdir -p "$3"; out=$(cd "$3" && pwd); shift 3
pages= langs="zh en" themes="light dark" kg= scenes=
while [ $# -gt 0 ]; do
    case $1 in
        --pages) pages=$2; shift ;;
        --lang) case $2 in zh|en) langs=$2 ;; both) langs="zh en" ;; *) echo "--lang zh|en|both"; exit 2 ;; esac; shift ;;
        --theme) case $2 in light|dark) themes=$2 ;; both) themes="light dark" ;; *) echo "--theme light|dark|both"; exit 2 ;; esac; shift ;;
        --keep-going) kg=--keep-going ;;
        *) scenes="$scenes $1" ;;
    esac
    shift
done
[ -n "$scenes" ] || scenes=$(ls "$models" | sed -nE 's/^index-(.*)-(light|dark)(-en)?\.txt$/\1/p' | sort -u)
names=
for s in $scenes; do for t in $themes; do for l in $langs; do
    n=$s-$t; [ "$l" = en ] && n=$n-en
    [ -f "$models/index-$n.txt" ] && names="$names $n"
done; done; done
[ -n "$names" ] || { echo "no runs in $models for that"; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
# no --pages: every page C shot in these runs
all=
[ -n "$pages" ] || all=$(for n in $names; do sed -n 's/^shot \([^ ]*\)\.[0-9]* .*/\1/p' "$models/index-$n.txt"; done | sort -u | tr '\n' , | sed 's/,$//')
docker run --rm -v u60-devui-rs-target:/t:ro -v "$models":/m:ro -v "$out":/o -v "$here/devui/testdata":/e:ro \
    -v "$LUYAZ_DEVICE_FONTS":/fonts/dev:ro -v "$LUYAZ_NUNITO_DIR":/fonts/nunito:ro \
    -e LUYAZ_UI_CJK_FONT=/fonts/dev/ZTEZhengYuan.ttf -e LUYAZ_UI_ROBOTO=/fonts/dev/Roboto.ttf \
    -e LUYAZ_UI_FONT_DIR=/fonts/nunito u60-rust-ci:1.99.0 sh -c '
pages=$1 kg=$2; shift 2
for n in "$@"; do
    ex=/e/expect.txt; case $n in *-en) ex=/e/expect-en.txt ;; esac
    # PNGs only of the shots that differ
    rm -rf /tmp/png; mkdir /tmp/png
    /t/release-device/luyaz-ui --render --models=/m --name=$n ${pages:+--pages=$pages} --png=/tmp/png --expect=$ex $kg 2>/dev/null >/tmp/o
    cat /tmp/o
    sed -n "s/^DIFF \([^ ]*\).*/\1/p" /tmp/o | while read -r s; do cp "/tmp/png/$s.png" /o/; done
done' sh "$pages" "$kg" $names >"$out/parity.log" || true
rc=0
grep -E '^(DIFF|NOTREACHED|CHECKS .* differ|TIMELINE|\[)' "$out/parity.log" || true
for s in $(sed -n 's/^DIFF \([^ ]*\).*/\1/p' "$out/parity.log"); do
    python3 -I "$here/pxcmp.py" "$ref/$s.png" "$out/$s.png" || true
    rc=1
done
grep -qE '^(CHECKS .* differ|TIMELINE|NOTREACHED|STOP)' "$out/parity.log" && rc=1
# every run finished (a crash prints no summary), and every page asked for was compared somewhere
runs=$(echo $names | wc -w)
done_runs=$(grep -c '^\[' "$out/parity.log" || true)
[ "$runs" -eq "$done_runs" ] || { echo "only $done_runs of $runs runs finished (see $out/parity.log)"; rc=1; }
for p in $(echo "${pages:-$all}" | tr , ' '); do
    grep -qE "^(SAME|DIFF) [^ ]*-$p\.[0-9]+( |\$)" "$out/parity.log" || { echo "page $p: no shot compared in any run"; rc=1; }
done
exit $rc
