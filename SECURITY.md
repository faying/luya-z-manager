# Security

## What this covers

The Luya-Z code in this repository: luyaz-core, luyaz-web (the web admin),
luyaz-sentinel, the touch UI and the install kit. Problems in ZTE firmware or
hardware belong to ZTE.

## Reporting

Please don't open a public issue. Use GitHub's private vulnerability reporting
(Security → Report a vulnerability) and include what is affected, how to
reproduce it, and what an attacker could do with it. Fixes are made privately
and published together with the advisory.

Only the newest release is maintained.

## Design notes worth knowing

- luyaz-web listens on `0.0.0.0:9090` for the device LAN. It needs a login (the
  password in `/data/luyaz/web.env`; without one every login is refused) and
  checks `Origin` on requests that change state. Don't expose it to the
  internet.
- luyaz-core listens on `127.0.0.1:9460`. Local processes on the device are
  trusted; LAN access goes through a token (`/data/luyaz/core/auth.token`).
- Keep SSH keys, passwords and tokens out of the repository.
