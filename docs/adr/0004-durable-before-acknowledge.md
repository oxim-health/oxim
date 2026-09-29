# 0004. Durable write before acknowledgment

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Clinical messages such as laboratory results must not be lost. When a device or system sends a message and receives a positive acknowledgment, it usually discards its copy. If the engine acknowledges before the message is safely stored and then crashes, the message is gone and nobody notices until a clinician asks for a missing result.

## Decision

We will send a positive acknowledgment to a source only after the message has been durably written (committed and flushed) to the message store.

- The default guarantee is at-least-once delivery to each destination.
- Group commit batches concurrent writes into a single transaction to keep throughput high.
- If the durable write fails, the source receives a negative acknowledgment (or no acknowledgment, where the protocol has none), so the sender retries.
- Duplicate detection on message control IDs and idempotency keys limits the effect of at-least-once redelivery.

## Consequences

- A crash at any point loses no acknowledged message.
- Acknowledgment latency includes a storage flush; fast storage and group commit are required to meet the latency targets.
- Destinations may receive a message more than once after a failure; duplicate handling is part of the design, not an afterthought.
- Chaos tests (process kill, fsync failure, full disk) are mandatory to prove the guarantee.
