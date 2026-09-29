# oxim-store-postgres

The PostgreSQL message store of [OXIM](../../README.md), for clusters of several OXIM nodes sharing one database (see ADR 0006 and the [cluster guide](../../docs/install/cluster.md)).

```yaml
# oxim.yaml
store:
  type: postgres
  url_env: OXIM_DATABASE_URL        # postgresql://oxim@db.lab.example/oxim
  tls: {ca_file: /etc/oxim/tls/db-ca.pem}
cluster:
  enabled: true
  node_id: oxim-1
```

- **Same semantics as SQLite.** Messages are durable before they are acknowledged (PostgreSQL's default `synchronous_commit = on`; do not lower it for OXIM's database), each destination has its own FIFO queue, and a failed delivery waits for an operator.
- **Shared queues.** Deliveries are claimed with `SELECT … FOR UPDATE SKIP LOCKED`, so with best-effort ordering every node sends from the same queue without sending a message twice. With strict ordering the head of a queue is locked, so only one node sends it and the next message waits for it.
- **Node ownership.** Every claim and every received message records the node. A node that restarts releases only its own in-flight deliveries; the node that takes over a dead node's channels adopts its claims and its received but unprocessed messages (`adopt_node`).
- **Migrations.** The schema is created and migrated on connect; nodes that start together serialize migrations with an advisory lock. Tables live in the connection's `search_path` schema.
- **TLS.** rustls with the ring provider, configured like the connectors' `tls` block.

The client is synchronous, because the engine calls the store from its own thread; `PostgresStore::connect` must not be called from inside an async task (use `spawn_blocking`).

## Testing

`cargo test -p oxim-store-postgres` runs against a real server when `OXIM_TEST_POSTGRES_URL` is set, for example `host=127.0.0.1 port=5432 user=postgres dbname=postgres`; each test uses its own schema. Without the variable the tests pass without checking anything.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
