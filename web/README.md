# Admin web — ZTE U60 Pro (MU5250)

Next.js static export, served by `zte-agent` at the site root (`http://<device>:9090/`). Works on phone, tablet and desktop, light and dark.

## Develop

```sh
npm ci
node scripts/mock-agent/server.ts      # typed mock agent on :9199 (no device needed)
npm run dev                             # then open http://localhost:3000
```

In the browser, set the agent URL on the login page ("Advanced") to `http://localhost:9199`; any password works against the mock. Mock scenarios (offline, 401, fake success, slow steps…) are listed in `scripts/mock-agent/README.md`.

Build for the device: `npm run build` → `out/`.

## Test

```sh
npm test               # vitest: api client, write-op state machine, helpers
npm run test:e2e       # Playwright against the built export + mock agent (see playwright.config.ts)
node scripts/contrast.ts > docs/contrast.md   # after any token change; must say 全部通过
```

## Where things are

| What | Where |
|---|---|
| Design rules (single source of truth) | `../docs/DESIGN.md` §5 |
| Tokens (colour, type, radii) | `src/app/newdesign.css` |
| Components | `src/components/nd/` (shell in `nd/shell/`) |
| Every route, nav, ⌘K aliases | `src/lib/routes.ts` |
| API client, freshness, write ops (confirm tiers, readback, steps) | `src/lib/api/` |
| Response types | `src/lib/api/schemas/` |
| Every control and datum per page, with its tier | `docs/controls-inventory.md` |
| Chinese strings for new-design pages | `src/lib/i18n/nd-zh/` (merged over `zh.ts`) |
