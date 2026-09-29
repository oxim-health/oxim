# 0010. QuickJS scripting and WebAssembly plugins

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Declarative mapping covers most transformations, but real integrations always need some custom logic. Integration engineers coming from Mirth Connect are used to writing JavaScript transformers. Heavier extensions, such as custom connectors or data types, need a stable interface that does not require forking OXIM. Both kinds of custom code are untrusted from the engine's point of view and must not be able to crash it or access data they should not.

## Decision

We will offer two extension mechanisms:

- **JavaScript scripting** in an embedded QuickJS runtime for filters and transformers. Scripts run in a sandbox with CPU-time and memory limits and no filesystem or network access unless explicitly granted. A compatibility layer emulates the E4X bracket-access style used by Mirth Connect scripts.
- **WebAssembly plugins** based on the component model for connectors, data types and transformers. Plugins are loaded at runtime, run sandboxed, and can be written in any language that compiles to WebAssembly.

## Consequences

- Familiar scripting for migrating users, with isolation the engine controls.
- Extensions do not require recompiling or forking OXIM.
- The scripting API and the plugin interface become public, versioned interfaces.
- Sandboxing adds overhead compared with native code; performance-critical connectors remain native Rust crates.
- E4X XML literal syntax cannot be supported; migrated scripts that use it need manual changes.
