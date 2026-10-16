# Security Policy

## Scope

This policy covers the Luya-Z code (luyaz-core, luyaz-web web admin, install kit; zte-agent was removed on 2026-10-14), not ZTE firmware or hardware — report those directly to ZTE.

## Reporting a Vulnerability

Report responsibly:

1. **Do not** open a public GitHub issue.
2. Open a private report via GitHub → Security → "Report a vulnerability", with:
   - A description of the vulnerability
   - Steps to reproduce
   - Potential impact
3. You'll get an acknowledgement within 72 hours.
4. A fix is developed privately and disclosed once a patch is available.

## Supported Versions

Only the latest release on the `main` branch is actively supported with security updates.

## Security Considerations

- The web admin (luyaz-web) binds to `0.0.0.0:9090` on the device LAN — it is intended for local network use only and should not be exposed to the internet. It requires a login (the admin password in `/data/luyaz/web.env`; with no password there it refuses every login) and checks `Origin` on writes.
- luyaz-core listens on `127.0.0.1:9460` only and does not authenticate local clients: anything running on the device can use it.
- SSH credentials and device access tokens should never be committed to the repository.
