# OXIM web UI

The web interface of [OXIM](../README.md), built with Svelte 5, TypeScript and Vite. The `oxim` program serves it from the same port as the REST API; release builds carry it inside the binary.

## Pages

| Page | Permission | What it does |
|---|---|---|
| Log in | public | User name and password; throttled logins show when to retry |
| Dashboard | `view_dashboard` | Totals and per-channel received, filtered, queued, sent and errored counts, live over server-sent events; system summary |
| Messages | `view_messages` | Search by channel, status, delivery status and time; keyset pagination; shareable URLs |
| Message detail | `view_messages` | Record, deliveries (retry now), and the raw, normalized, transformed, reply, encoded and response contents side by side as text, fields (HL7 v2, ASTM) or a tree (JSON); masked by default with audited break-glass access; reprocess and erase with a reason |
| Channels | `view_channels` | Channel files, deployment state and queues; deploy, undeploy, redeploy, delete |
| Channel editor | `edit_channels` | YAML editor (CodeMirror 6) with the server's validation errors shown inline and a live structured view of source, filters, transformers and destinations |
| Code tables | `view_tables` | List, and a grid editor (`edit_tables`) with the server's validation rules applied while typing; raw CSV mode |
| Users | `manage_users` | Add users, change role, name and disabled state, set passwords, end sessions |
| API tokens | `manage_tokens` | Create (shown once) and revoke tokens |
| Audit log | `view_audit` | Filter by action, actor and message |
| System health | `view_system` | Version, uptime, TLS, sessions, deployed channels, installed components |
| Account | any | Own permissions and password change |

Alerts, Devices and Backups appear as placeholders ("not available in this build") until the server offers an API for them.

## Design rules

- **Air-gapped:** no CDN, web fonts or telemetry; everything is bundled.
- **Strict Content-Security-Policy:** the server sends `default-src 'self'`, so there are no inline scripts or style attributes. CodeMirror runs in a shadow root, where its styles are constructable style sheets that the policy allows. The end-to-end tests fail on any policy violation.
- **Accessibility (WCAG 2.1 AA):** landmarks and a skip link, labelled controls, keyboard operation, focus moved to the page heading on navigation, modal dialogs on the native `<dialog>` element with focus returned to the opener, status never shown by color alone, contrast checked in light and dark themes, reduced motion respected. axe-core runs in the end-to-end tests.
- **One typed client:** `src/lib/api/schema.d.ts` is generated from `openapi.json` (the server's OpenAPI document). Paths, bodies and responses are type-checked; changes repeat the CSRF cookie in `X-CSRF-Token`.

## Development

Node 22 or newer.

```sh
npm ci
npm run dev        # Vite dev server; proxies /api to OXIM at http://127.0.0.1:8080 (OXIM_URL overrides)
npm run lint       # ESLint
npm run check      # svelte-check (TypeScript)
npm test           # Vitest unit tests
npm run build      # production build in dist/
```

Rebuild OXIM after `npm run build` to embed the new `dist/`, or point `server.ui_dir` in `oxim.yaml` at `ui/dist` to serve it without rebuilding.

### API types

`openapi.json` is a copy of the server's document, kept current by a Rust test. After changing the API:

```sh
OXIM_BLESS=1 cargo test -p oxim-server --test openapi   # refresh ui/openapi.json
npm run gen:api                                         # regenerate src/lib/api/schema.d.ts
```

### End-to-end tests

The Playwright tests start a real `oxim run` with a temporary configuration, an administrator and a viewer, and one synthetic HL7 message received over MLLP, then drive the UI in Chromium.

```sh
npx playwright install chromium   # once
npm run build
npm run e2e                        # serves ui/dist through server.ui_dir
OXIM_E2E_EMBEDDED=1 npm run e2e    # uses the UI embedded in target/debug/oxim
```

The harness builds `target/debug/oxim` with cargo when it is missing; set `OXIM_BIN` to use another binary. All data is synthetic.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
