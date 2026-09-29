# oxim-connectors-messaging

Messaging connectors for [OXIM](../../README.md) channels. `oxim_connectors_messaging::register(&mut registry)` adds them; the `oxim` program registers them by default.

| Type | System | Source | Destination |
|---|---|---|---|
| `mqtt` | MQTT 3.1.1 brokers | subscription; QoS 1 messages acknowledged after storage | publish; QoS 1 waits for `PUBACK` |
| `amqp` | AMQP 0-9-1 (RabbitMQ) | queue consumer; ack after storage | publish with publisher confirms |
| `kafka` | Apache Kafka, Redpanda | partition reader with an offsets file | produce with `acks=all` |
| `nats` | NATS, NATS JetStream | core subscription or JetStream durable consumer | core publish or JetStream publish with acknowledgment |

Every source acknowledges a message to its broker only after OXIM stored it durably, so stopping OXIM (or a crash) never loses a message; it may arrive twice, delivery is at-least-once. Every destination reports success only once the broker confirmed the message; otherwise the delivery is retried according to the destination's retry policy.

TLS uses rustls with the ring provider and the `tls` settings of the stream connectors (`ca_file`, `system_roots`, `cert_file`, `key_file`, `server_name`; see [oxim-connectors](../oxim-connectors/README.md#tls)). Passwords and tokens come from `*_env` settings (environment variables read when connecting) or, discouraged, inline.

Topics, subjects, routing keys and message keys may contain `{...}` placeholders filled from the delivery: `lab/{MSH-3}/results`, `{$message_id}`, `{PID-3.1}` (see `oxim_connectors::values`). A delivery without a value for a placeholder fails.

## `mqtt`

A client written for OXIM: an inbound QoS 1 message is acknowledged only after it is stored, and the broker keeps unacknowledged messages for the next connection. With a stable `client_id` and `clean_session: false` (the default), messages published while OXIM is offline are delivered when it reconnects.

Source:

| Setting | Default | Meaning |
|---|---|---|
| `broker` | required | `host:port` (1883, or 8883 with TLS) |
| `topics` | required | Topic filters, `+` and `#` allowed |
| `qos` | `1` | 0 (at most once) or 1 (at least once) |
| `client_id` | `oxim-<channel>` | |
| `clean_session` | `false` | |
| `keep_alive` | `30s` | |
| `username`, `password`, `password_env` | none | |
| `tls` | none | |
| `connect_timeout` | `10s` | |
| `max_message_len` | 16 MiB | |

Destination: `broker`, `topic` (template), `qos` (`1`), `retain` (`false`), `client_id` (`oxim-<channel>-<destination>`), `keep_alive`, `username`, `password`, `password_env`, `tls`, `connect_timeout`, `ack_timeout` (`30s`).

Messages carry `mqtt.topic` metadata. MQTT 5 and QoS 2 are not used.

```yaml
source:
  type: mqtt
  data_type: json
  settings:
    broker: mqtt.hospital.internal:8883
    topics: [poct/+/results]
    username: oxim
    password_env: OXIM_MQTT_PASSWORD
    tls: {ca_file: /etc/oxim/tls/hospital-ca.pem}
```

## `amqp`

Source:

| Setting | Default | Meaning |
|---|---|---|
| `url` | required | `amqp://host:5672/vhost` or `amqps://host:5671/vhost` (`%2f` is `/`) |
| `queue` | required | The queue to consume (it must exist) |
| `prefetch` | `10` | Unacknowledged messages the broker may send ahead |
| `consumer_tag` | `oxim-<channel>` | |
| `username`, `password`, `password_env` | from the URL | Override the URL's user information |
| `tls` | none | For `amqps://`: `ca_file` and a client certificate; the URL host is verified |
| `connect_timeout` | `10s` | |

A message that cannot be stored is returned to the queue (`nack` with requeue). Messages carry `amqp.exchange`, `amqp.routing_key`, `amqp.message_id` and text headers as `amqp.header.<name>`; the `correlation_id` property becomes the correlation identifier.

Destination:

| Setting | Default | Meaning |
|---|---|---|
| `url` | required | |
| `exchange` | `""` | The default exchange routes by queue name |
| `routing_key` | `""` | Template |
| `mandatory` | `true` | An unroutable message fails the delivery instead of being dropped by the broker |
| `persistent` | `true` | Delivery mode 2 |
| `content_type` | from the data type | |
| `username`, `password`, `password_env`, `tls`, `connect_timeout` | | As for the source |
| `confirm_timeout` | `30s` | Time to wait for the publisher confirm |

Published messages carry the OXIM message identifier as `message_id` and the channel as the `oxim-channel` header.

## `kafka`

Source:

| Setting | Default | Meaning |
|---|---|---|
| `brokers` | required | Bootstrap brokers, `host:port` |
| `topic` | required | |
| `partitions` | all | Partitions to read |
| `start` | `earliest` | Where partitions without a stored offset start: `earliest` or `latest` |
| `offsets_file` | required | JSON file with the next offset of each partition, written after each stored batch |
| `max_wait` | `500ms` | Longest wait of one fetch |
| `max_bytes` | 1 MiB | Most bytes fetched at once per partition |
| `client_id` | `oxim-<channel>` | |
| `tls` | none | |
| `sasl` | none | `{mechanism: plain \| scram-sha-256 \| scram-sha-512, username, password_env}` |
| `timeout` | `30s` | Limit for connecting and for retrying failed requests |

Kafka consumer groups are not used: the source reads its partitions directly and keeps its position in `offsets_file`, so one channel reads each partition. Offsets are written after a batch is stored; after a crash the last batch may be read again. When the stored offset has expired (retention), reading continues at the earliest kept record. Messages carry `kafka.topic`, `kafka.partition`, `kafka.offset`, `kafka.timestamp` (milliseconds) and text headers; a text key becomes the correlation identifier.

Destination: `brokers`, `topic`, `partition` (`0`), `key` (template), `compression` (`none`, `gzip`, `lz4`, `snappy`, `zstd`), `client_id`, `tls`, `sasl`, `timeout`. Records carry the `oxim-message-id` and `oxim-channel` headers. Records that are too large or invalid fail the delivery at once.

## `nats`

Source:

| Setting | Default | Meaning |
|---|---|---|
| `servers` | required | `nats://host:4222` (or `tls://`) |
| `subject` | required without `jetstream` | Subject to subscribe to; with `jetstream`, the filter of a consumer OXIM creates |
| `queue_group` | none | Core NATS queue group |
| `jetstream` | none | `{stream, consumer}`: read a durable pull consumer (created with explicit acknowledgment if missing) |
| `username`, `password`, `password_env` | none | |
| `token`, `token_env` | none | |
| `credentials_file` | none | NATS credentials file (JWT and NKey seed) |
| `tls` | none | The server address is verified |
| `connect_timeout` | `10s` | |

Core NATS does not keep messages for subscribers that are offline; use JetStream when every message counts. With JetStream each message is acknowledged (and the acknowledgment confirmed) after it is stored; a message that cannot be stored is negatively acknowledged for redelivery. A core NATS request with a reply subject gets the channel's reply when the channel answers requests (`source.response`). Messages carry `nats.subject`, `nats.stream` and text headers.

Destination: `servers`, `subject` (template), `jetstream` (`false`: publish and flush; `true`: wait for the stream's acknowledgment), authentication and `tls` as for the source, `connect_timeout`, `ack_timeout` (`30s`). Messages carry `Nats-Msg-Id` (the OXIM message identifier, so JetStream discards duplicates of a retried delivery) and `Oxim-Channel` headers.

## Testing against real brokers

`cargo test -p oxim-connectors-messaging` runs the MQTT connectors against an in-process broker and checks settings and message mapping for the others. Round trips against real brokers run when these variables are set:

```text
OXIM_TEST_MQTT=127.0.0.1:1883
OXIM_TEST_AMQP=amqp://guest:guest@127.0.0.1:5672/%2f
OXIM_TEST_KAFKA=127.0.0.1:9092
OXIM_TEST_NATS=nats://127.0.0.1:4222   # with JetStream enabled
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
