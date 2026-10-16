#!/bin/sh
# Pass line 6: renaming a field the templates use must break the build (askama checks templates
# against the Rust struct at compile time). Renames SignalView.word → word_renamed in a scratch
# copy of the crate and expects `cargo check` to fail on the template. Run in the Rust container:
#   tools/u60-dev rust sh /w/manager/crates/luyaz-web/e2e/rename-check.sh
set -eu
SRC=/w/manager/crates/luyaz-web
T=$(mktemp -d)
cp -r "$SRC" "$T/luyaz-web"
mkdir -p "$T/crates" && mv "$T/luyaz-web" "$T/crates/luyaz-web"
cd "$T/crates/luyaz-web"
printf '[workspace]\n' >> Cargo.toml
sed -i 's/pub word: &.static str,/pub word_renamed: \&'"'"'static str,/; s/^        word,$/        word_renamed: word,/' src/view.rs
grep -q 'word_renamed' src/view.rs || { echo "rename did not apply"; exit 2; }
if CARGO_TARGET_DIR=/w/manager/target-web cargo check --offline -q 2>"$T/err"; then
  echo "FAIL: build passed after renaming a template field"; rm -rf "$T"; exit 1
fi
grep -m1 -E 'no field `word`|word' "$T/err" | sed 's/^/  /'
echo "PASS: renaming SignalView.word breaks the build"
rm -rf "$T"
