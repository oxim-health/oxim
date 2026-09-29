# 0003. Sans-IO protocol libraries

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM implements several stateful clinical protocols (MLLP, ASTM LIS01/LIS02, POCT1-A, DICOM upper layer). The same protocol logic is needed in different settings:

- actively, over real TCP sockets and serial ports inside the engine;
- passively, when analyzing captured traffic (for example in the companion netKit project or in shadow mode);
- in tests and simulators, where real devices and networks are unavailable.

Protocol code that performs its own I/O or reads the system clock cannot be reused across these settings and is hard to test deterministically.

## Decision

We will write every protocol crate in the sans-IO style.

- A protocol implementation consumes bytes and produces bytes and events. It never opens sockets, touches files or spawns tasks.
- Time is passed in by the caller. Timers are expressed as deadlines that the caller must honor.
- Protocol crates declare `#![forbid(unsafe_code)]` and depend on no async runtime.
- I/O adapters live in `oxim-connectors`.

## Consequences

- Protocol behavior (timeouts, retries, contention) can be tested deterministically without a network.
- The same code serves the engine, simulators and passive analysis.
- The protocol crates are useful on their own and can be published independently.
- The connector layer must correctly drive the state machines and honor their deadlines, which adds some glue code.
