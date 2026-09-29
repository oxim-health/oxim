# oxim-auth

Users, roles, sessions, API tokens and login throttling for [OXIM](../../README.md).

- **Roles:** `admin`, `operator` and `viewer`, each a fixed set of permissions (below). Roles apply to every channel; per-channel grants are planned.
- **Passwords:** Argon2id with 19 MiB of memory, 2 iterations and parallelism 1 (the OWASP baseline), a random salt per password, stored as a PHC string. At least 12 characters and different from the user name.
- **Sessions:** random 256-bit tokens (`oxs_…`); only their SHA-256 hash is stored. Each session has an idle timeout (default 30 minutes) and an absolute timeout (default 12 hours), a CSRF token for cookie-based use, logout, and revoke-all per user. Disabling a user, demoting them or changing their password ends their sessions.
- **API tokens:** named, random 256-bit tokens (`oxt_…`) with a role of their own and an optional expiry; stored hashed, shown once, revocable.
- **Throttling:** failed logins are limited per user name and per client address (10 per 15 minutes), in memory with a bounded number of tracked keys.
- **No account enumeration:** unknown users cost the same password check as known ones, and a disabled account is only reported once the password is right.
- **Constant-time comparisons** for tokens and CSRF values.

Everything lives in `auth.db`, a SQLite database with the same durability settings as the message store (WAL, `synchronous=FULL`) and schema migrations through `user_version`.

## Permissions

| Permission | admin | operator | viewer |
|---|:---:|:---:|:---:|
| `view_dashboard`, `view_channels`, `view_tables`, `view_system` | ✓ | ✓ | ✓ |
| `view_messages` (patient-identifying values masked) | ✓ | ✓ | ✓ |
| `view_unmasked` | ✓ | ✓ | |
| `repair_messages` (reprocess, requeue) | ✓ | ✓ | |
| `edit_channels`, `deploy_channels`, `edit_tables` | ✓ | ✓ | |
| `erase_messages` | ✓ | | |
| `manage_users`, `manage_tokens`, `view_audit` | ✓ | | |

Viewers can still see unmasked content in an emergency through the server's break-glass endpoint, which requires a reason and is always audited.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
