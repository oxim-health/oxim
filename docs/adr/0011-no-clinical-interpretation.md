# 0011. No clinical interpretation

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Integration software sits between devices and clinical systems. Software that computes, verifies or flags clinical results (for example auto-verification or reference-range evaluation) takes part in clinical decisions. Such software carries much higher safety responsibility and may fall under medical device regulation in many jurisdictions. Software that only transfers, stores and converts data generally carries a different responsibility.

## Decision

OXIM moves and reformats data. It does not interpret it.

- OXIM ships no rules that compute, verify or flag clinical results.
- Transformations that change a value (for example unit conversion) are explicit, visible in the channel configuration, and owned by the user who configures them. OXIM never applies them implicitly.
- Result flags and reference ranges supplied by a device are carried through unchanged unless the user explicitly maps them.
- Users can write their own scripts; responsibility for any clinical logic in those scripts lies with the user.

This is a design principle, not legal advice. Each deployment is responsible for assessing its own regulatory position.

## Consequences

- A clear scope that keeps the product focused on integration.
- Some features users may request (auto-verification, delta checks) are out of scope and belong in a LIS or dedicated middleware.
- Documentation must explain this boundary so users understand where responsibility lies.
