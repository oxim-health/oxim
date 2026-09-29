# oxim-connectors-remote

Remote file, object storage, email and web service connectors for [OXIM](../../README.md). Register them with `oxim_connectors_remote::register(&mut registry)` (the `oxim` program does this); channel files then use the type names below.

| Type | Source | Destination |
|---|---|---|
| `sftp` | Remote directory poller over SFTP | Atomic file writer over SFTP |
| `ftp` | Remote directory poller over FTP/FTPS | Atomic file writer over FTP/FTPS |
| `s3` | Object poller for S3-compatible storage | Object writer |
| `smtp` | | Email with the message as attachment or body |
| `imap` | Emails, or their attachments, from a mailbox folder | |
| `soap` | SOAP 1.1/1.2 endpoint | SOAP 1.1/1.2 client with WS-Security and OAuth 2.0 |

Common rules:

- Sources remove, move or flag a remote item only after the message is stored durably (at least once: an item stored just before a crash is read again).
- Destinations report temporary failures (retried by the destination's retry policy) and permanent ones.
- Connector factories never open connections, so `oxim validate` works offline. Secrets are read from environment variables (`*_env` settings) when a connection is opened; inline secrets are accepted but discouraged.
- TLS uses rustls with the ring provider and the `tls` settings of `oxim-connectors` (`ca_file`, `system_roots`, `cert_file`/`key_file` for client certificates, `server_name`). Certificates are always verified. Plain FTP, SMTP and IMAP must be chosen explicitly with `security: none`.
- Durations are written like `500ms`, `5s`, `2m`; sizes in bytes.

## Remote directories: `sftp`, `ftp`, `s3`

These share the polling and writing settings of the local `file` connector.

Source:

| Setting | Default | Meaning |
|---|---|---|
| `directory` | login directory (bucket root for `s3`) | Remote directory (for `s3`: key prefix) to watch |
| `pattern` | `*` | File names to pick up; `*` and `?` wildcards, ASCII case-insensitive |
| `poll_interval` | `5s` | Time between listings |
| `sort` | `name` | `name` or `modified` |
| `after` | `move` | `move` or `delete` |
| `processed_directory` | none | Where stored files are moved (required for `move`) |
| `error_directory` | none | Where files larger than `max_file_size` are moved |
| `max_file_size` | 64 MiB | Largest accepted file |

A file is picked up once its size and modification time stayed the same between two listings. Files whose names start with `.` are ignored. Moved files get a free name (`result-1.hl7` becomes `result-1-1.hl7` when taken).

Destination:

| Setting | Default | Meaning |
|---|---|---|
| `directory` | login directory | Remote directory (for `s3`: key prefix) |
| `filename` | `{channel}-{message_id}.{extension}` | File name template (`{channel}`, `{destination}`, `{message_id}`, `{timestamp}`, `{extension}`) |
| `overwrite` | `false` | Replace an existing file with the same name |
| `create_directory` | `true` | Create missing directories |

Files are uploaded under a hidden temporary name and renamed, so readers never see partial files (S3 writes objects atomically without a temporary key). Writing the same message again succeeds when the existing file has the same content; a different existing file fails the delivery permanently unless `overwrite` is set. A kept connection that the server closed while idle is reopened once before an attempt fails.

### `sftp`

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | Server |
| `port` | `22` | Port |
| `username` | required | User |
| `password_env` / `password` | none | Password |
| `private_key_file` | none | OpenSSH or PEM private key (Ed25519 or ECDSA) |
| `private_key_passphrase_env` | none | Passphrase of the key |
| `known_hosts_file` | none | `known_hosts` file listing the server key |
| `host_key_fingerprint` | none | SHA-256 fingerprint of the server key (`SHA256:...`, as `ssh-keygen -lf` prints it) |
| `connect_timeout` | `10s` | Connecting and authenticating |
| `timeout` | `60s` | Each file operation |

The server key is always verified: exactly one of `known_hosts_file` and `host_key_fingerprint` is required, and unknown or changed keys refuse the connection. RSA host and client keys are not supported: the Rust RSA implementation SSH would need carries an unresolved timing side-channel advisory (RUSTSEC-2023-0071).

```yaml
source:
  type: sftp
  data_type: hl7v2
  settings:
    host: files.hospital.example
    username: lab-results
    private_key_file: /etc/oxim/keys/lab-results_ed25519
    known_hosts_file: /etc/oxim/keys/known_hosts
    directory: outbound/results
    pattern: '*.hl7'
    processed_directory: outbound/done
```

### `ftp`

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | Server |
| `port` | `21` (`990` for implicit TLS) | Port |
| `security` | `explicit` | `explicit` (FTPS with `AUTH TLS`), `implicit` or `none` |
| `tls` | system roots | TLS settings |
| `username` | `anonymous` | User |
| `password_env` / `password` | none | Password |
| `passive` | `pasv` | `pasv` or `epsv` |
| `connect_timeout` | `10s` | Connecting, TLS and login |
| `timeout` | `60s` | Each file operation |

Data connections are passive and protected (`PROT P`) with FTPS; the data address announced by the server is replaced by the control connection's address, as NAT requires. Listings use `MLSD` and fall back to `LIST`.

### `s3`

| Setting | Default | Meaning |
|---|---|---|
| `endpoint` | required | For example `https://s3.eu-central-1.amazonaws.com` or `https://minio.lab.internal:9000` |
| `region` | `us-east-1` | Signing region |
| `bucket` | required | Bucket |
| `path_style` | `false` | `endpoint/bucket/key` addressing (MinIO and most self-hosted stores) |
| `access_key_id_env`, `secret_access_key_env` | none | Access keys |
| `session_token_env` | none | Session token of temporary credentials |
| `tls` | system roots | TLS settings |
| `timeout` | `60s` | Each request |

Requests are signed with AWS Signature Version 4 (implemented in this crate and checked against the published AWS test vectors); payloads are always signed. Objects are listed one level below the prefix (`delimiter=/`); `after: move` copies the object and deletes the original.

## `smtp` (destination)

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | SMTP server |
| `port` | `587`, `465` for `tls`, `25` for `none` | Port |
| `security` | `starttls` | `starttls` (required, never falls back to plain text), `tls` or `none` |
| `tls` | system roots | TLS settings |
| `username`, `password_env` / `password` | none | SMTP authentication |
| `from` | required | Sender, for example `OXIM <oxim@lab.example.org>` |
| `to`, `cc` | required, none | Recipient lists |
| `subject` | `OXIM message {message_id}` | Subject template |
| `content` | `attachment` | `attachment` or `body` (UTF-8 text) |
| `attachment_name` | `{channel}-{message_id}.{extension}` | Attachment file name template |
| `text` | `Message {message_id} from channel {channel}.` | Body text when the message is attached |
| `hello_name` | host name | Name sent with `EHLO` |
| `timeout` | `30s` | One delivery |

The `Message-ID` is derived from the OXIM message identifier, so receivers can recognize a repeated delivery. Permanent SMTP errors (5xx, such as an unknown recipient) fail the delivery; authentication failures and other errors are retried.

## `imap` (source)

| Setting | Default | Meaning |
|---|---|---|
| `host` | required | IMAP server |
| `port` | `993` for `tls`, else `143` | Port |
| `security` | `tls` | `tls`, `starttls` (required) or `none` |
| `tls` | system roots | TLS settings |
| `username`, `password_env` / `password` | required | Login |
| `folder` | `INBOX` | Folder |
| `search` | `UNSEEN` | IMAP `SEARCH` criteria |
| `content` | `message` | `message` (the whole email) or `attachments` (each matching attachment) |
| `attachment_pattern` | `*` | Attachment names to take |
| `after` | `seen` | `seen`, `delete` or `move` |
| `move_to` | none | Target folder for `move` |
| `poll_interval` | `60s` | Time between checks |
| `max_message_size` | 25 MiB | Larger emails are skipped |
| `connect_timeout`, `timeout` | `10s`, `60s` | Limits |

Emails are read with `BODY.PEEK[]` and flagged, deleted or moved only after every message taken from them is stored. Messages carry `mail.subject`, `mail.from`, `mail.message_id`, `imap.uid` and, for attachments, `file.name` metadata.

## `soap`

Destination:

| Setting | Default | Meaning |
|---|---|---|
| `url` | required | Service endpoint |
| `version` | `1.1` | `1.1` or `1.2` |
| `action` | none | SOAP action |
| `envelope` | `wrap` | `wrap` (the message is the Body content) or `none` (the message is a full envelope) |
| `payload_element`, `payload_namespace` | none | Element holding a non-XML message (such as HL7 v2) as escaped text |
| `headers` | none | Extra HTTP headers |
| `ws_security` | none | UsernameToken: `{username, password_env, password_type: digest}` (`digest` or `text`) |
| `oauth2` | none | OAuth 2.0 client credentials (see `oxim-connectors`) |
| `tls` | system roots | TLS settings |
| `timeout` | `30s` | The exchange |
| `max_response_size` | 16 MiB | Largest response |
| `response` | `body` | Stored response: `body` content or whole `envelope` |

Faults with a `Client`/`Sender` code fail the delivery permanently; other faults are retried. HTTP errors without a fault are classified like the `http` destination.

Source:

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on |
| `path` | `/` | Accepted path prefix |
| `max_body` | 16 MiB | Largest request |
| `max_connections` | `100` | Concurrent connections |
| `store` | `body` | What becomes the message: `body` content or `envelope` |
| `acknowledgment` | `<oxim:Acknowledgment>` with the message id | Body content answered once stored (`{message_id}` is replaced) |
| `reply_element` | `oxim:Reply` | Element holding a non-XML reply |
| `auth` | none | `{type: basic, ...}` or `{type: bearer, ...}` |
| `tls` | none | TLS listener settings |

The endpoint answers in the SOAP version of the request. With `source.response` the channel's reply becomes the response Body. Malformed requests get a `Client`/`Sender` fault; storage failures a `Server`/`Receiver` fault so callers retry. MTOM/XOP attachments and WS-Security verification on the endpoint are not supported yet.

## SMB/CIFS shares

No SMB connector type is provided: no maintained pure-Rust SMB2/3 client fits OXIM's dependency policy yet (the available crate conflicts with the SSH stack and needs a large authentication dependency tree). Mount the share on the host and use the local `file` connector:

- Linux: `mount -t cifs //fileserver/lab /mnt/lab -o credentials=/etc/oxim/smb.cred,uid=oxim,gid=oxim` (or an `/etc/fstab` entry), then `directory: /mnt/lab/results`.
- Windows: run the service under an account with access to the share and use the UNC path, for example `directory: '\\fileserver\lab\results'`.

## Testing

The tests run against in-process servers: an SSH server with an SFTP subsystem, an FTP/FTPS server, an S3-compatible endpoint that verifies every signature, SMTP and IMAP servers, and SOAP services. `tests/live.rs` additionally checks real services when `OXIM_LIVE_SFTP`, `OXIM_LIVE_FTP`, `OXIM_LIVE_S3`, `OXIM_LIVE_SMTP` or `OXIM_LIVE_SOAP` holds destination settings; see the file for examples. All test data is synthetic.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
