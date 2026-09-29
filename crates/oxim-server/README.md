# oxim-server

The REST API of [OXIM](../../README.md): authentication, channels, messages with patient-data masking, code tables, users, API tokens, the audit trail, Prometheus metrics and live dashboard events. The `oxim` program starts it with the engine (`server:` in `oxim.yaml`).

- **Listens on `127.0.0.1:8080` by default.** HTTPS with rustls when `server.tls` names a PEM certificate and key; TLS handshakes run in the background with a timeout.
- **Refuses to start without users:** create the first administrator with `oxim users create-admin`.
- **Authentication:** `POST /api/v1/auth/login` returns a session token and sets an `HttpOnly; Secure; SameSite=Strict` session cookie plus a readable CSRF cookie. Browsers use the cookie and repeat the CSRF value in the `X-CSRF-Token` header for every change (double submit); scripts send the session token or an API token as `Authorization: Bearer …`.
- **Masking:** callers without `view_unmasked` get patient-identifying values replaced by `***`, keeping the structure — HL7 v2 (PID-2…23 identifiers, names, birth date, address, phones, SSN, …; NK1, MRG, GT1, IN1/IN2, PV1-19/50), ASTM patient records, `patient`/`subject` objects of normalized and FHIR JSON, and POCT1-A `PT.*` values. Content that cannot be masked reliably is withheld. Message metadata values are masked too.
- **Audit:** every content view (masked or not), break-glass access, erasure, channel, table, user and token change and every login is recorded in the message store's audit trail. Content is only returned after its audit event is stored.
- **Hardening:** security headers (CSP, `nosniff`, `DENY` framing, no referrer, HSTS with TLS), `Cache-Control: no-store` for the API, request body limit (4 MiB), request timeout (30 s), bounded event streams, JSON errors `{"error": {"code", "message"}}`.
- **Web UI:** with the `embedded-ui` feature (on by default in the `oxim` program) the built UI in `ui/dist` (or the directory named by `OXIM_UI_DIST` at build time) is compiled into the binary and served with its content types, `Cache-Control: public, max-age=31536000, immutable` for content-hashed `assets/` and `no-cache` for everything else, and `index.html` for client-side routes. `server.ui_dir` overrides the embedded files. A build without the UI answers browsers with a page explaining how to add it and other clients with a JSON note at `/`. Building OXIM never needs Node: when `ui/dist` is missing the build simply embeds nothing.

## API

The OpenAPI 3.1 description is served at `/api/v1/openapi.json`, with JSON schemas for every request and response body. Tests check that every documented operation is routed and requires authentication, that real responses match their schemas, and that the copy in `ui/openapi.json` (from which the web UI generates its TypeScript types) is current; refresh it with `OXIM_BLESS=1 cargo test -p oxim-server --test openapi`, then `npm run gen:api` in `ui/`.

| Method | Path | Permission | Purpose |
|---|---|---|---|
| POST | `/api/v1/auth/login` | public | Log in; returns token, CSRF token and cookies (throttled) |
| POST | `/api/v1/auth/logout` | any | End the session |
| GET | `/api/v1/auth/me` | any | Current user or token and its permissions |
| POST | `/api/v1/auth/password` | any (users) | Change one's own password |
| GET | `/api/v1/channels` | `view_channels` | Channel files, deployment state, queue depths |
| GET | `/api/v1/channels/{id}` | `edit_channels` | A channel's YAML (it may hold credentials such as HTTP headers) |
| PUT | `/api/v1/channels/{id}` | `edit_channels` | Validate with the component registry and save atomically; the file watcher redeploys |
| DELETE | `/api/v1/channels/{id}` | `edit_channels` + `deploy_channels` | Undeploy and move the file to `channels_dir/.deleted/` |
| POST | `/api/v1/channels/{id}/deploy`, `/undeploy`, `/redeploy` | `deploy_channels` | Control a channel |
| GET | `/api/v1/messages` | `view_messages` | Search (`channel`, `status`, `destination_status`, `from`, `until`, `limit`); keyset pagination with `before` / `next_before` |
| GET | `/api/v1/messages/{id}` | `view_messages` | Record, destination states and stored contents |
| GET | `/api/v1/messages/{id}/content` | `view_messages` | One stage (`stage`, `destination`); masked unless `view_unmasked`; audited |
| POST | `/api/v1/messages/{id}/content/unmasked` | `view_messages` (users) | Break-glass: unmasked content with a mandatory reason; audited |
| POST | `/api/v1/messages/{id}/reprocess` | `repair_messages` | Process again from the raw content |
| POST | `/api/v1/messages/{id}/requeue` | `repair_messages` | Retry a failed delivery now |
| POST | `/api/v1/messages/{id}/erase` | `erase_messages` | Delete with a mandatory reason; audited |
| GET | `/api/v1/tables`, `/api/v1/tables/{name}` | `view_tables` | Code tables (CSV) |
| PUT | `/api/v1/tables/{name}` | `edit_tables` | Validate and save a code table |
| GET, POST | `/api/v1/users` | `manage_users` | List and create users |
| PATCH | `/api/v1/users/{username}` | `manage_users` | Display name, role, disabled (the last admin stays) |
| POST | `/api/v1/users/{username}/password` | `manage_users` | Set a password |
| DELETE | `/api/v1/users/{username}/sessions` | `manage_users` | Log a user out everywhere |
| GET, POST | `/api/v1/tokens` | `manage_tokens` | List and create API tokens (shown once) |
| DELETE | `/api/v1/tokens/{id}` | `manage_tokens` | Revoke a token |
| GET | `/api/v1/audit` | `view_audit` | Audit trail (`message`, `action` prefix, `actor`, `limit`) |
| GET | `/api/v1/system` | `view_system` | Version, uptime, deployed channels, component types |
| GET | `/api/v1/events` | `view_dashboard` | Server-sent `stats` events (counts per channel and status) |
| GET | `/api/v1/openapi.json` | public | The OpenAPI document |
| GET | `/metrics` | `view_system` | Prometheus metrics; give Prometheus a viewer API token |

## Metrics

`oxim_messages{channel,status}`, `oxim_deliveries{channel,destination,status}`, `oxim_queue_oldest_pending_seconds{channel,destination}`, `oxim_channel_deployed{channel}`, `oxim_http_responses_total{class}`, `oxim_login_failures_total`, `oxim_uptime_seconds` and `oxim_build_info{version}`. Counts come from the message store at scrape time.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
