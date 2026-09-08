# imap-safe-mail

A safety-first mailbox API for agents and applications.

Its public mutation vocabulary is deliberately small:

- `trash(message)` moves a message into the server's mailbox marked `\\Trash`.
- `restore(message, destination)` moves a trashed message back to a normal mailbox.

It intentionally has no `expunge`, permanent-delete, or raw `\\Deleted` API. This mirrors the familiar recoverable-trash behaviour of mail clients while keeping destructive IMAP primitives outside the application boundary.

It fetches raw RFC 822 messages via the Rust [`imap`](https://crates.io/crates/imap) client with Rustls TLS and certificate verification. The public wrapper permits only `LIST`, `SELECT`, and `UID FETCH ... BODY.PEEK[]`; `BODY.PEEK[]` avoids changing the `\\Seen` flag. Fetched messages project to N-Triples or N-Quads using the same `https://mail.described.at/` and `http://schema.org/` vocabulary as `mbox-rdf`.

## Status

This repository provides the domain model, a deterministic in-memory store, a TLS-protected read-only IMAP client, and an RDF projection. `mail-shapes.ttl` is copied from `mbox-rdf` so both projects use the same SHACL contract. The next layer is a production mutation adapter that discovers the `\\Trash` mailbox and uses `MOVE` when available.

## Quick example

```rust
use imap_safe_mail::{InMemoryMailStore, MailboxRole, MailStore, MessageId};

let mut mail = InMemoryMailStore::new();
mail.add_mailbox("INBOX", MailboxRole::Inbox);
mail.add_mailbox("Trash", MailboxRole::Trash);
mail.add_message("INBOX", MessageId::new("42"))?;

mail.trash(&MessageId::new("42"))?;
mail.restore(&MessageId::new("42"), "INBOX")?;
# Ok::<(), imap_safe_mail::MailError>(())
```

## CLI: export recent mail as N-Quads

The CLI always uses Rustls TLS with certificate verification and reads an authentication secret from an environment variable—never from a command-line argument. It supports password `LOGIN`, SASL `PLAIN`, widely deployed `XOAUTH2`, and RFC 7628 `OAUTHBEARER`.

```bash
export IMAP_AUTH_SECRET='your app password'
cargo run --release -- fetch \
  --host imap.gmail.com \
  --username you@gmail.com \
  --auth login \
  --secret-env IMAP_AUTH_SECRET \
  --mailbox INBOX \
  --limit 50 \
  --graph urn:email:you@gmail.com \
  --data-iri https://data.example.com/ \
  --include-body \
  --output recent-mail.nq
```

Omit `--include-body` to export metadata only. The default graph is `urn:email:<username>`.

For a provider-issued OAuth access token, select the matching SASL mechanism instead of a password:

```bash
export IMAP_AUTH_SECRET='access-token'
cargo run --release -- fetch \
  --host imap.example.com --username you@example.com \
  --auth oauthbearer --secret-env IMAP_AUTH_SECRET \
  --output recent-mail.nq
```

The CLI intentionally does not acquire or refresh OAuth tokens: authorization endpoints, client registration, and scopes are provider-specific. Supply a valid short-lived token from your existing identity/OAuth workflow.

## Design constraints

- A message must have exactly one current mailbox in this API.
- Trash is a normal mailbox with the special `\\Trash` role, not the IMAP `\\Deleted` flag.
- Restore cannot target a Trash mailbox.
- The implementation records auditable `Trashed` and `Restored` events.
- The public trait has no operation capable of permanent deletion.

## Development

```bash
cargo test
```

### GreenMail integration test

The default tests are hermetic. The opt-in integration test starts a local GreenMail container, injects a message over SMTP, then logs into GreenMail over IMAPS and fetches it through `MailReader`.

```bash
docker compose -f docker-compose.greenmail.yml up -d
cargo test --features greenmail-tests --test greenmail -- --test-threads=1
docker compose -f docker-compose.greenmail.yml down -v
```

GreenMail's bundled certificate is deliberately self-signed. The one test-only constructor that bypasses certificate validation exists only under `greenmail-tests`; production builds retain Rustls certificate verification.
