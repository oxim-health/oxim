# 0005. Per-destination queues

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM receives messages from many devices and systems and delivers them to one or more destinations. A single shared queue would be simple, but one failing destination or one message the receiver keeps rejecting would block every other flow behind it (head-of-line blocking). In a laboratory, that means a misconfigured archive could stop results from reaching the LIS.

## Decision

We will give every channel/destination pair its own ordered (FIFO) queue.

- Order is preserved within a queue, so results from one device reach a destination in the order they arrived.
- A failure, retry backoff or rejected message affects only its own queue.
- All queues live in the same store and are visible in the same dashboard.
- Users can deliberately route several sources into a single channel when they want one shared queue.
- Real-time host queries bypass the queues, because devices expect an answer within seconds.

## Consequences

- Failures are isolated and easy to locate.
- Ordering is guaranteed per queue, not globally across queues.
- The system manages many queues, which requires efficient storage indexing and a scheduler that scales with the number of queues.
