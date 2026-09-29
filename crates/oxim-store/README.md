# oxim-store

Durable message storage and per-destination delivery queues for [OXIM](../../README.md).

- **Acknowledge only what is stored:** `receive` commits messages durably (SQLite WAL, `synchronous=FULL`) before the sender is acknowledged.
- **One queue per destination:** a failing destination never blocks the others. Queues are strictly first-in, first-out by default, or best-effort when order does not matter.
- **Retries and dead letters:** failed attempts are retried at a caller-chosen time; deliveries that give up wait for an operator to requeue them.
- **Crash recovery:** in-flight deliveries return to their queues and unprocessed messages are listed after a restart (at-least-once delivery).
- **Every stage kept:** raw, normalized, transformed, encoded and response content is stored for inspection and reprocessing until a prune policy removes it.
- **Audit trail and erasure:** an append-only audit table, and deletion of messages by identifier for erasure requests.

`SqliteStore` implements the `MessageStore` trait for single-node deployments.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
