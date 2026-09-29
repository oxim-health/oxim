# 0012. MIT OR Apache-2.0 licensing

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM aims to be used and embedded widely: by hospitals, by integration consultants and by commercial LIS and HIS vendors. A copyleft license would prevent many vendors from embedding it. The Rust ecosystem standard is dual licensing under MIT and Apache-2.0, which combines MIT's simplicity with Apache-2.0's explicit patent grant.

## Decision

We will license all OXIM code under **MIT OR Apache-2.0**, at the user's option.

- Contributions are accepted under the same dual license, as stated in `CONTRIBUTING.md`.
- Dependencies must use licenses compatible with this choice; cargo-deny enforces an allow list in CI.
- Separate repositories under the project (device profiles, channel templates) use the same license unless a record states otherwise.

## Consequences

- Commercial and closed-source products can embed OXIM, which increases adoption.
- Contributors receive the patent protection of Apache-2.0.
- Others may build proprietary products on OXIM without contributing back; we accept this in exchange for broad adoption.
- Dependencies with copyleft licenses cannot be used in the core.
