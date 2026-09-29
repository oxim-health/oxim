# 0013. English-only project content

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

OXIM targets hospitals, laboratories and vendors worldwide. Contributors and users will come from many countries. Mixing languages across code, documentation and issues fragments the project and excludes contributors.

## Decision

All project content is in English: source code, identifiers, comments, commit messages, README files, documentation, notes, issue and pull request templates, and the UI.

- The UI is translation-ready (strings externalized), so community translations can be added without code changes.
- Examples and test fixtures may contain non-English data where needed to test character-set handling, but all surrounding text is in English.

## Consequences

- A single language for all contributors and users.
- Documentation in other languages depends on community translation.
