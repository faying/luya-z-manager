#!/bin/sh
# Nothing changes, nothing is redrawn: replay each C run on the Rust screen (render --still), then
# leave every tab on the last model for 5 refreshes; any invalidated area is a failure (it would
# wake the device's quiet loop every second; manager docs/designs/u60-touch-rewrite.md §7).
#
#   rs/still.sh MODEL_DIR [scene…]     (MODEL_DIR as rs/parity.sh; fonts as rs/parity.sh)
#
# Exempt: scenes whose screen shows a running countdown (事务行「正在确认 · 1:26」), which really
# changes every second.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
set -eu
[ $# -ge 1 ] || { sed -n 2,6p "$0"; exit 2; }
models=$(cd "$1" && pwd); shift
names=
if [ $# -gt 0 ]; then
    for s in "$@"; do names="$names $(ls "$models" | sed -nE "s/^index-($s-(light|dark)(-en)?)\.txt$/\1/p")"; done
else
    names=$(ls "$models" | sed -nE 's/^index-(.*)\.txt$/\1/p')
fi
docker run --rm -v u60-devui-rs-target:/t:ro -v "$models":/m:ro \
    -v "$LUYAZ_DEVICE_FONTS":/fonts/dev:ro -v "$LUYAZ_NUNITO_DIR":/fonts/nunito:ro \
    -e LUYAZ_UI_CJK_FONT=/fonts/dev/ZTEZhengYuan.ttf -e LUYAZ_UI_ROBOTO=/fonts/dev/Roboto.ttf \
    -e LUYAZ_UI_FONT_DIR=/fonts/nunito u60-rust-ci:1.99.0 sh -c '
for n in "$@"; do /t/release-device/luyaz-ui --render --models=/m --name=$n --pages=none --still 2>/dev/null | grep "^STILL"; done' sh $names >/tmp/still.$$ || true
LIVE='op-verifying op-rollback-off'
bad=$(awk -v live=" $LIVE " '{ s = $2; sub(/^\[/, "", s); sub(/\/.*/, "", s) } $4 != 0 && index(live, " " s " ") == 0' /tmp/still.$$)
runs=$(awk '{print $2}' /tmp/still.$$ | sort -u | wc -l | tr -d ' ')
rm -f /tmp/still.$$
[ -z "$bad" ] || { printf '%s\n' "$bad" | cut -c1-200; echo "still: redraws with nothing changed"; exit 1; }
want=$(echo $names | wc -w | tr -d ' ')
[ "$runs" = "$want" ] || { echo "still: only $runs of $want runs reported (crashed or stopped?)"; exit 1; }
echo "still: $runs runs, every tab quiet"
