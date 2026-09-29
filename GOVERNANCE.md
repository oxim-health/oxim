# Governance

This document describes how decisions are made in the OXIM project.

## Roles

- **Contributors** are anyone who opens issues, reviews pull requests or submits changes.
- **Maintainers** review and merge pull requests, triage issues and publish releases. They are listed in [MAINTAINERS.md](MAINTAINERS.md).
- **Lead maintainer** breaks ties when maintainers cannot reach consensus.

A contributor with a sustained record of high-quality contributions may be invited to become a maintainer by consensus of the existing maintainers.

## Decision making

- Day-to-day changes are decided in pull request review. A pull request needs approval from at least one maintainer who is not its author.
- Architecture decisions are recorded as ADRs in [`docs/adr`](docs/adr). Changing an accepted decision requires a new ADR that supersedes it.
- Maintainers seek consensus. If consensus cannot be reached, the lead maintainer decides and records the reasoning.

## RFC process

Significant changes, such as new public APIs, changes to the channel configuration schema, the plugin interface, the storage format or the security model, follow this process:

1. Open an issue labeled `rfc` describing the problem, the proposal, alternatives and migration impact.
2. Allow at least 14 days for discussion.
3. A maintainer summarizes the outcome. Accepted RFCs are recorded as ADRs before implementation starts.

## Releases

OXIM publishes no release before the complete 1.0 ([ADR 0014](docs/adr/0014-single-complete-1-0-release.md)). After 1.0, releases follow semantic versioning for the public Rust APIs, the channel configuration schema and the plugin interface.

## Code of conduct

All participants are expected to follow the [code of conduct](CODE_OF_CONDUCT.md).
