# 0002. Rust with embedded C components

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM parses untrusted input from devices and networks, stores protected health information and must run for months without restarts on modest hardware, often on Windows servers inside hospitals. Memory-safety bugs in parsers are a well-known source of vulnerabilities in network and medical software. Existing integration engines commonly run on the JVM, which brings high memory use and a separate runtime to install and patch.

Two mature C components solve problems we should not solve again: SQLite (embedded, transactional storage) and QuickJS (a small, embeddable JavaScript engine).

## Decision

We will implement OXIM in Rust.

- SQLite and QuickJS are embedded as C libraries through well-maintained Rust bindings, compiled into the binary.
- `unsafe` code is allowed only at FFI boundaries (bindings to C libraries and the `oxim-ffi` C ABI). Protocol crates declare `#![forbid(unsafe_code)]`.
- No C++ is used in the core.

## Consequences

- Memory safety for all parsing and processing code, without garbage-collection pauses.
- A single self-contained binary with no runtime to install.
- The build requires a C compiler for the embedded components, which complicates cross-compilation slightly.
- Security updates to SQLite or QuickJS require an OXIM release, because the libraries are compiled in.
- The contributor pool is smaller than for Java or Go; good documentation and approachable code matter more.
