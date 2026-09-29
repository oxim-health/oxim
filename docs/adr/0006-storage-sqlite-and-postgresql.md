# 0006. Storage: SQLite and PostgreSQL

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

Most OXIM installations are single servers or small PCs in a laboratory or clinic, often without a database administrator. Requiring a separate database server would make installation and maintenance much harder. Large hospitals, however, need high availability and horizontal scale, which requires shared storage between nodes.

## Decision

We will support two storage backends behind a single storage interface in `oxim-store`:

- **SQLite** (WAL mode, embedded) is the default for single-node installations. It requires no installation, stores everything in one file, and is easy to back up. Full-text search uses SQLite FTS5.
- **PostgreSQL** is used for cluster mode. It also serves as the coordination layer for the cluster (leases, advisory locks, `SKIP LOCKED` queue claims), so no additional coordination service such as etcd or ZooKeeper is needed.
- Large attachments can be stored in a blob store (filesystem, NFS or S3-compatible) referenced from either backend.

## Consequences

- Zero-dependency installation for the common case.
- Cluster mode reuses a database that hospitals already know how to operate.
- Every storage feature must be implemented and tested on both backends.
- Moving from SQLite to PostgreSQL requires a migration tool, which becomes part of the scope.
