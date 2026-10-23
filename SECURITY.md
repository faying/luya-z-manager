# Security policy

## Scope

This policy covers the Luya-Z code here: luyaz-core, the luyaz-web admin, luyaz-sentinel, the touch
UI and the install kit. Bugs in ZTE's firmware or hardware are ZTE's to fix.

## How to report

Not in a public issue, please. Report it with GitHub's private vulnerability reporting (Security →
Report a vulnerability): what is affected, steps to reproduce, and what an attacker gains. The fix
is prepared privately and released together with the advisory. Only the latest release gets fixes.

## Things to know about the design

- luyaz-web serves the device LAN on `0.0.0.0:9090`. Every page needs a login (password in
  `/data/luyaz/web.env`; with no password set, all logins are refused), and state-changing requests
  must carry a matching `Origin`. Never put it on the internet.
- luyaz-core serves `127.0.0.1:9460`. Processes on the device are trusted; LAN clients need the
  token in `/data/luyaz/core/auth.token`.
- SSH keys, passwords and tokens never belong in the repository.
