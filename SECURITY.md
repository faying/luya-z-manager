# Security Policy

## Scope

This policy covers the open-u60-pro toolkit code (zte-agent, web admin, install kit), not ZTE firmware or hardware — report those directly to ZTE.

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

- `zte-agent` binds to `0.0.0.0:9090` on the device LAN — it is intended for local network use only and should not be exposed to the internet.
- No authentication is implemented by default; the agent trusts all clients on the local network.
- SSH credentials and device access tokens should never be committed to the repository.
