# 0014. Single complete 1.0 release

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

A clinical integration engine is only useful when its core promises hold together: guaranteed delivery, full administration, security and device connectivity. A partial public release would invite production use of an incomplete system in environments where lost or corrupted messages affect patients. The trade-off is that a long period without public releases delays real-world feedback.

## Decision

We will publish no public release before the complete 1.0 described in the specification.

- The repository is public from the first day; development happens in the open.
- Work proceeds through internal milestones M0 to M8, each ending with a passing test suite.
- From milestone M4 onward, internal pilot builds run at real sites to collect feedback before 1.0.
- 1.0 is released only when every acceptance criterion in the specification is met.

## Consequences

- Users never meet a half-finished product under a release version.
- Feedback before 1.0 comes from pilot sites and simulators rather than from the general public, so pilot sites must be chosen deliberately.
- Protocol crates are not published to crates.io before 1.0 either, which delays external reuse.
- Scope discipline is essential: the specification defines 1.0, and additions go through an ADR.
