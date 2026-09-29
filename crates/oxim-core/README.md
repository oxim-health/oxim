# oxim-core

The channel runtime of [OXIM](../../README.md).

A channel connects one source connector to any number of destinations:

```text
source ──submit──▶ store (durable) ──▶ processor ──▶ per-destination queues ──▶ destination workers
  ▲                      │               parse, normalize, filter,     │                   │
  └──── acknowledge ◀────┘               transform, encode              └── retry policy ◀──┘
```

- **Acknowledge after storing:** `SourceContext::submit` returns only once the message is durable; the store thread commits concurrent arrivals together (group commit).
- **Pipeline:** parse into a lossless `Document` (HL7 v2, ASTM, POCT1-A, JSON, XML, delimited, fixed width), optionally normalize to the clinical model, run channel and per-destination filters and transformers, and encode per destination. Step failures mark the message as errored for reprocessing; they never lose it.
- **Queues and retries:** one worker per destination, strict or best-effort ordering, exponential backoff, permanent rejections, give-up limits.
- **Recovery:** in-flight deliveries return to their queues and unprocessed messages are processed after a crash or redeploy.
- **Configuration:** channels are YAML files (`ChannelConfig`); connector and step types are resolved by name through a `Registry`, so any crate can contribute components.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
