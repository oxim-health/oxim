# oxim-connectors-db

Database connectors for [OXIM](../../README.md) channels. `oxim_connectors_db::register(&mut registry)` adds them; the `oxim` program registers them by default.

| Type | Database | Source | Destination |
|---|---|---|---|
| `postgres` | PostgreSQL | polling reader | writer |
| `mysql` | MySQL, MariaDB | polling reader | writer |
| `mssql` | Microsoft SQL Server, Azure SQL | polling reader | writer |
| `sqlite` | SQLite database files | polling reader | writer |

ODBC and Oracle are not supported yet: both need native client libraries that OXIM does not bundle.

## Statements and parameters

Statements are written with named parameters, `:name`, for every database. OXIM rewrites them to the database's own placeholders (`$1`, `?`, `@P1`) and binds each value as a parameter; message content is never pasted into SQL text. Quoted strings, quoted identifiers, comments and PostgreSQL `::` casts are left alone. Positional placeholders written by hand are rejected.

Values are sent as text and converted by the database to the parameter's type, so `'5.40'` becomes a `numeric(6,2)` exactly and `'2026-09-29 12:00:00'` a timestamp.

## Polling reader (source)

| Setting | Default | Meaning |
|---|---|---|
| `query` | required | The query that selects new rows |
| `params` | none | Constant values of the query's parameters, for example `{site: LAB1}` |
| `post_query` | none | Runs after each row is stored, with the row's columns (then `params`) as parameters; typically marks the row as processed |
| `interval` | `5s` | Time between polls; a full batch is followed by the next poll at once |
| `max_rows` | `500` | Most rows taken per poll |
| `column` | none | Use the raw content of this column as the message instead of the row as JSON |

Each row becomes one message: a JSON object from column names to values, in column order. Exact numerics (`numeric`, `decimal`) stay text so no digit is lost, dates and times are ISO 8601 (`2026-09-29`, `2026-09-29T12:00:00.5Z`), binary values are Base64 and JSON columns are embedded as JSON. Set the channel's `data_type: json`.

The row is stored durably before `post_query` runs. If OXIM stops between the two, the row is read again: delivery is at-least-once, never lossy. A query should therefore select only unprocessed rows.

```yaml
id: lis-orders-from-db
source:
  type: postgres
  data_type: json
  settings:
    host: lis-db.hospital.internal
    database: lis
    user: oxim
    password_env: OXIM_LIS_DB_PASSWORD
    tls: {ca_file: /etc/oxim/tls/hospital-ca.pem, system_roots: false}
    query: SELECT id, specimen, test, requested_at FROM orders WHERE site = :site AND exported = false ORDER BY id
    params: {site: LAB1}
    post_query: UPDATE orders SET exported = true WHERE id = :id
    interval: 10s
```

## Writer (destination)

| Setting | Default | Meaning |
|---|---|---|
| `statement` | required | A statement, or a list of statements run in order in one transaction |
| `params` | none | The source of every statement parameter |

Parameter sources:

| Written as | Value |
|---|---|
| `$payload` | The delivered message as text |
| `$payload_bytes` | The delivered message as binary data (`bytea`, `BLOB`, `VARBINARY`) |
| `$message_id`, `$channel`, `$destination`, `$data_type`, `$attempts` | Delivery fields |
| `{value: LAB1}` | A constant |
| anything else | A path into the delivered message: `MSH-10`, `PID-3.1`, `OBX[2]-5`, `order.id` |

A path without a value binds `NULL`. Every statement parameter needs a source and every source must be used, so typos fail at deploy time.

Connection failures and other database errors (deadlocks, a missing table) are retried according to the destination's retry policy. Constraint and data errors (duplicate keys, `NOT NULL`, invalid values) fail the delivery at once, because the same statement cannot succeed; the transaction is rolled back. A payload that cannot be parsed for a path fails the delivery too.

```yaml
destinations:
  - id: results-archive
    type: mssql
    settings:
      host: sql.hospital.internal
      database: integration
      user: oxim
      password_env: OXIM_SQL_PASSWORD
      statement:
        - INSERT INTO lab_messages (message_id, control_id, mrn, body) VALUES (:id, :control, :mrn, :body)
        - UPDATE lab_counters SET received = received + 1 WHERE site = :site
      params:
        id: $message_id
        control: MSH-10
        mrn: PID-3.1
        body: $payload
        site: {value: LAB1}
```

## Connections

Passwords come from `password_env`, an environment variable read when a connection opens, or from `password` (discouraged: it stores the secret in the channel file). Connection details in logs never include secrets.

### `postgres`

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | Server name or address; also the name the certificate must match |
| `port` | `5432` | |
| `database`, `user` | required | |
| `password`, `password_env` | none | |
| `tls` | none (plain) | TLS settings (`ca_file`, `system_roots`, `cert_file`, `key_file`); TLS is required when set |
| `connect_timeout` | `10s` | |
| `application_name` | `oxim` | Shown in `pg_stat_activity` |

Types without a conversion (arrays, network addresses, geometry) are Base64 of PostgreSQL's binary format; cast them to `text` in the query for a readable value.

### `mysql`

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | |
| `port` | `3306` | |
| `database`, `user` | required | |
| `password`, `password_env` | none | |
| `tls` | none (plain) | TLS settings; `system_roots` selects the driver's built-in Mozilla root certificates, `server_name` overrides the name to verify |
| `connect_timeout` | `10s` | |

### `mssql`

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | |
| `port` | `1433` | Named instances need their fixed port |
| `database`, `user` | required | SQL Server authentication |
| `password`, `password_env` | none | |
| `encryption` | `strict` | `strict`: TDS 8.0 strict encryption, TLS before the login with the certificate verified (SQL Server 2022 and later, Azure SQL); `none`: no encryption at all, for isolated networks only |
| `tls` | system roots | TLS settings for `strict` |
| `connect_timeout` | `10s` | |

Older SQL Server versions that only offer TLS inside the TDS login cannot be reached encrypted: the Rust TDS client's own TLS support pulls a TLS stack OXIM does not ship. Use SQL Server 2022 strict encryption, a TLS tunnel, or `encryption: none` on an isolated network. Result sets are read completely; limit large tables with `TOP` in the query.

### `sqlite`

| Setting | Default | Meaning |
|---|---|---|
| `path` | required | The database file |
| `busy_timeout` | `5s` | How long to wait for a lock held by another process |
| `create` | `false` | Create the file when it does not exist |

## Testing against real servers

`cargo test -p oxim-connectors-db` runs everything with SQLite. The PostgreSQL, MySQL and SQL Server round trips run when environment variables hold connection settings as a YAML flow mapping:

```text
OXIM_TEST_POSTGRES="{host: 127.0.0.1, database: oxim, user: oxim, password: secret}"
OXIM_TEST_MYSQL="{host: 127.0.0.1, database: oxim, user: oxim, password: secret}"
OXIM_TEST_MSSQL="{host: 127.0.0.1, database: oxim, user: sa, password: secret, encryption: none}"
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
