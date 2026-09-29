# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM is a long-lived open-source project that handles clinical data. Contributors join over time, and many design choices (storage, delivery semantics, protocol boundaries) are expensive to reverse. Without a written record, the reasoning behind a decision is lost, and the same questions get reopened repeatedly.

## Decision

We will record every significant architecture decision as an Architecture Decision Record (ADR) in `docs/adr/`, using the lightweight template in `0000-template.md`.

- Records are numbered sequentially and never renumbered.
- An accepted record is not edited in substance. A changed decision gets a new record, and the old one is marked "Superseded by NNNN".
- A decision is significant if it affects public APIs, data formats, delivery guarantees, security, licensing or the project's scope.
- Pull requests that make such a decision include the corresponding ADR.

## Consequences

- New contributors can learn why the system is built the way it is.
- Reopening a decision requires writing down what changed, which filters out churn.
- Writing records adds a small amount of overhead to significant changes.
