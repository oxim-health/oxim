# 0008. Channels as YAML files

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Integration channels are critical configuration. Hospitals need to review changes, move channels between test and production environments, and roll back mistakes. Engines that keep channel definitions inside their own database make version control, code review and diffing awkward.

## Decision

We will store channel definitions as YAML files in a configuration directory.

- The YAML schema is versioned and documented.
- The web UI and the API read and write the same files; there is no second source of truth.
- Environment-specific values (hosts, ports, credentials) are expressed as variables and secret references, so the same channel file works across environments.
- OXIM keeps a version history of each channel with diff and rollback, independent of whether the user also uses git.
- Channels can be deployed and undeployed individually without restarting the engine.

## Consequences

- Channels can be versioned in git, reviewed in pull requests and promoted between environments.
- Changes made outside the UI (for example by editing files) must be detected and validated before deployment.
- The YAML schema becomes a public interface with its own compatibility rules.
