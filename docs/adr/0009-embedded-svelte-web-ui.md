# 0009. Embedded Svelte web UI

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Operators need a complete administration interface: dashboard, message browser, channel designer, device registry, users and audit log. Desktop administration clients must be installed and updated on every workstation. The UI must also work on air-gapped networks, so it cannot depend on external services or CDNs.

Options considered were a Rust-only frontend (for example Leptos) and a TypeScript frontend. A TypeScript frontend has a larger contributor pool and more mature tooling for complex UI such as code editors and flow designers.

## Decision

We will build the web UI with Svelte and TypeScript.

- The compiled UI is embedded in the OXIM binary and served locally; it has no runtime dependency on external resources.
- The UI talks only to the public REST API and WebSocket endpoints; it has no private backend interface.
- The UI is in English and translation-ready, and targets WCAG 2.1 AA.

## Consequences

- No client installation; any modern browser on the hospital network can administer OXIM.
- Everything the UI can do is available to API users as well.
- The build requires a Node.js toolchain in addition to Rust; releases ship the prebuilt UI so users never need it.
- Frontend contributions follow separate tooling and testing (Playwright end-to-end tests).
