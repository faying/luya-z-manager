#!/bin/sh
# Run one browser check in place: e2e/<name>.mjs resolves @playwright/test and axe from
# crates/luyaz-web/node_modules (crates/luyaz-web/package.json; `npm ci` there once).
#   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 AGENT=http://127.0.0.1:19090 \
#     OUT=/tmp/lw crates/luyaz-web/e2e/run.sh step5
set -eu
here=$(cd "$(dirname "$0")" && pwd)
name=${1:?usage: run.sh <check> (shell proxy trial topbar opbar signal-en step5 session batch2 batch3 …)}
[ -f "$here/$name.mjs" ] || { echo "no check $here/$name.mjs" >&2; exit 2; }
[ -d "$here/../node_modules/@playwright/test" ] || { echo "no node_modules: (cd $(dirname "$here") && npm ci)" >&2; exit 2; }
exec node "$here/$name.mjs"
