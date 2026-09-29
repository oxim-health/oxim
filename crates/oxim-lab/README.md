# oxim-lab

Laboratory workflows for [OXIM](../../README.md): an order cache, host query answers, test routing and worklist download between a LIS and its analyzers.

Analyzers ask "what should I run on tube S123?" and expect an answer within seconds, while the LIS may be slow or down. OXIM caches every order it forwards and answers these host queries itself. Results mark the cached tests as done, so a rerun query does not repeat finished work.

`oxim_lab::register(&mut registry, LabEnvironment::new("data/orders.db", "tables"))` adds:

| Type | Kind | Purpose |
|---|---|---|
| `cache-orders` | transformer | files normalized `Orders` in the cache |
| `answer-query` | transformer | turns a host `Query` into the cached `Orders` for the queried tubes |
| `record-results` | transformer | marks cached tests as resulted |
| `select-tests` | transformer | keeps only the tests one device performs, one order per tube |
| `has-tests-for` | filter | keeps orders that concern one device |

All steps work on the normalized model (`normalize: true`) and pass other content unchanged. They never change results and never interpret clinical values ([ADR 0011](../../docs/adr/0011-no-clinical-interpretation.md)).

## The order cache

Orders are filed by specimen identifier (the tube barcode): the specimen's first identifier, else the order's first specimen reference. Each requested test has a status:

| Status | Meaning |
|---|---|
| `pending` | requested, not yet given to a device |
| `sent` | handed to a device: queued as a worklist download or returned in a query answer |
| `resulted` | a result arrived |
| `cancelled` | cancelled by the requester |

| Order control | Effect |
|---|---|
| new | adds tests that are not cached yet; known tests keep their status, so a retransmitted order changes nothing; cancelled tests are requested again |
| add | like new, and resulted tests are requested again: reruns and reflex tests |
| replace | makes the open tests exactly the listed ones; others are cancelled |
| cancel | cancels the listed tests, or every open test when none are listed |

The cache lives in its own SQLite file (`orders.db` next to `oxim.db`). It is derived data: reprocessing the stored order messages rebuilds it. `retention.orders_after` deletes orders that have not changed for a while (90 days by default).

## Routing tables

A CSV table in the tables directory says which devices perform which tests:

```csv
test,device,note
GLU,chem-1,
GLU,chem-2,backup analyzer
CREA,chem-1,
HGB,hema-1,
```

A test may be performed by several devices, listed in order of preference. A test matches a row when any of its codes does, so both the LIS code and the device code work after `map-observations`.

## Load balancing

Without further settings every device that performs a test receives it in its worklist. To spread the work instead, give `cache-orders` the routing table and a strategy; each open test is then assigned to one device, and `select-tests`/`has-tests-for` send it to that device only:

```yaml
transformers:
  - {type: cache-orders, routing: routing.csv, balance: least_loaded}
```

| `balance` | The test goes to |
|---|---|
| `primary` (default) | the first device listed for it |
| `round_robin` | the listed devices in turn, per test code |
| `least_loaded` | the device with the fewest open tests in the cache (ties go to the first listed) |

A device that asks for a tube in a host query is still offered every test it performs, and the assignment follows the tube.

## Host queries

The analyzer's connection carries queries and results. The reply encoder only answers the messages it handles (orders produced by `answer-query`), so results are acknowledged without a reply.

```yaml
id: chem-1
source:
  type: astm-tcp
  data_type: astm
  normalize: true
  response:
    mode: pipeline
    timeout: 10s
    encoder: {type: astm-query-response, sender: OXIM}
  settings: {listen: 0.0.0.0:5001}
transformers:
  - {type: map-observations, table: chem-1-to-lis.csv}
  - {type: answer-query, device: chem-1, routing: routing.csv}
  - {type: record-results, device: chem-1}
  - {type: map-observations, table: lis-to-chem-1.csv}
destinations:
  - id: lis
    type: mllp
    filters: [{type: clinical-kind, kinds: [results, quality_control]}]
    encoder: {type: hl7v2-oru-r01, sending_application: OXIM}
    settings: {target: lis.example.org:2575}
```

`answer-query` settings:

| Setting | Default | Meaning |
|---|---|---|
| `device` | none | the device, recorded on the offered tests and used for routing |
| `routing` | none | a routing table; only tests routed to `device` are offered |
| `include_resulted` | `false` | also offer tests that already have results |
| `mark_sent` | `true` | record the offered tests as sent |
| `on_missing` | `answer` | `ask`: leave queries for tubes the cache does not know unanswered, for a `source.response.fallback` to the LIS |

HL7 analyzers that follow IHE Laboratory Analytical Workflow send `QBP^Q11` over MLLP. `hl7v2-rsp-k11` answers with `RSP^K11` (`QAK` status `OK` or `NF`). IHE LAW then expects the orders as a separate `OML^O33`, which a destination to the analyzer sends; devices that expect the orders inside the response use `include_orders: true`:

```yaml
source:
  type: mllp
  data_type: hl7v2
  normalize: true
  response:
    mode: pipeline
    encoder: {type: hl7v2-rsp-k11}
  settings: {listen: 0.0.0.0:2577}
transformers:
  - {type: answer-query, device: immuno-1, routing: routing.csv}
destinations:
  - id: immuno-1
    type: mllp
    filters: [{type: clinical-kind, kinds: [orders]}]
    encoder: {type: hl7v2-oml-o33}
    settings: {target: 10.0.0.31:2575}
```

For each queried tube the answer holds the open tests (pending or sent) that the device performs and that the query asked for. Unknown tubes and tubes with nothing left are left out, so `astm-query-response` answers "no information" (`L|1|I`).

## Asking the LIS about unknown tubes

When the cache does not know a tube (the order has not arrived yet, or OXIM was installed after it was placed), the query can be passed to the LIS within the reply timeout. `answer-query` with `on_missing: ask` leaves such a query unanswered, the LIS destination receives only unanswered queries, and `source.response.fallback` turns the LIS's answer into the device's reply:

```yaml
id: chem-1
source:
  type: astm-tcp
  data_type: astm
  normalize: true
  response:
    mode: pipeline
    timeout: 10s
    encoder: {type: astm-query-response, sender: OXIM}
    fallback:
      destination: lis-query
      data_type: hl7v2                 # the LIS answers RSP^K11 with the orders
      transformers:
        - {type: cache-orders, mark_sent: true, device: chem-1}
        - {type: map-observations, table: lis-to-chem-1.csv}
  settings: {listen: 0.0.0.0:5001}
transformers:
  - {type: answer-query, device: chem-1, routing: routing.csv, on_missing: ask}
destinations:
  - id: lis-query
    type: mllp
    filters: [{type: clinical-kind, kinds: [query]}]
    encoder: {type: hl7v2-qbp-q11, sending_application: OXIM, receiving_application: LIS}
    settings: {target: lis.example.org:2575}
```

The LIS gets `QBP^Q11` (IHE LAW work order query, QPD-3 the tube); its `RSP^K11` answer with PID/SPM/ORC/OBR segments is normalized into orders, cached and encoded for the analyzer. A "not found" answer (`QAK-2 NF`, no orders) gives the analyzer "no information". If the LIS does not answer within the timeout, the analyzer gets no reply and asks again; the query stays queued for the LIS and its late answer is still cached.

## Worklist download

Orders from the LIS are cached once and sent to each analyzer that performs some of their tests:

```yaml
id: lis-orders
source:
  type: mllp
  data_type: hl7v2
  normalize: true
  settings: {listen: 0.0.0.0:2576}
transformers:
  - {type: cache-orders}
destinations:
  - id: chem-1
    type: astm-tcp
    filters: [{type: has-tests-for, device: chem-1, routing: routing.csv}]
    transformers: [{type: select-tests, device: chem-1, routing: routing.csv}]
    encoder: {type: astm-orders}
    settings: {listen: 0.0.0.0:5001}
```

The ASTM destination names the same endpoint as the analyzer's query channel, so both share one LIS01 link to the analyzer.

`select-tests` merges the groups of one tube into one order, since analyzers expect one order record per tube, and records the selected tests as sent (`mark_sent: false` turns this off). A cancellation without tests reaches every device that performs one of the tube's tests.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
