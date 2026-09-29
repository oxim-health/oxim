# Contributing to OXIM

Thank you for your interest in OXIM. This document explains how to propose changes and what we expect from a contribution.

## Before you start

- Read the [specification](docs/SPEC.md) and the [architecture decision records](docs/adr). Changes that contradict an accepted ADR need a new ADR first.
- For anything larger than a bug fix or a small improvement, open an issue to discuss the approach before writing code. Significant changes go through the RFC process described in [GOVERNANCE.md](GOVERNANCE.md).
- All project content (code, comments, commit messages, documentation, UI text) is written in English ([ADR 0013](docs/adr/0013-english-only-project-content.md)).

## Patient data

**Never include real patient data** in issues, pull requests, test fixtures, logs or screenshots. This includes names, identifiers, dates of birth and free-text results, even if you believe they are anonymized. Use synthetic data only. Device captures must be passed through `oxim-anonymize` and reviewed by a maintainer before they are accepted.

## Development workflow

1. Fork the repository and create a branch from `main`.
2. Make your change with tests. Protocol code needs unit tests, and parsers need property or fuzz coverage for untrusted input.
3. Run the same checks as CI:

   ```sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   cargo deny check
   ```

4. Open a pull request that explains what changed and why, and link the related issue.

## Code guidelines

- Keep the existing style: `rustfmt` formatting, documented public items, no `unsafe` outside FFI boundaries (protocol crates forbid it).
- Code that handles input from devices or networks must never panic on malformed data and must bound memory use.
- Protocol crates are sans-IO: no sockets, files, threads or clocks ([ADR 0003](docs/adr/0003-sans-io-protocol-libraries.md)).
- Prefer small, focused pull requests. Refactors and behavior changes belong in separate commits.
- Write commit messages in the imperative mood with a short summary line (for example `hl7: keep CRLF endings when inserting segments`).

## Dependencies

New dependencies must be permissively licensed and pass `cargo deny check`. Explain in the pull request why a new dependency is needed.

## License of contributions

OXIM is dual licensed under the MIT license and the Apache License 2.0. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in OXIM by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
