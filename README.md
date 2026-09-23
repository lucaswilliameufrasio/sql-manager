# SQL Manager

A lightweight native SQL client built with Rust and egui. PostgreSQL is the first
target, with an architecture planned to support additional database engines.

## Development

Install the Rust toolchain, then run the desktop application with:

```sh
cargo run
```

The app currently supports saved PostgreSQL connection profiles, system-keyring
password storage, asynchronous connection tests over verified TLS by default,
encrypted connection backup and restore, a persistent SQL workspace with a
1,000-row result cap, schema/table browsing, and paged table views with visual row
insert, update, and delete when the table has a primary key. The remaining milestones
include extension to other database engines. SSH tunnels use the system OpenSSH
client and support keys from the SSH agent or a selected identity file. Visual schema
operations can create, rename, and restrict-drop tables and add, rename, and
restrict-drop columns; column types come from a fixed allowlist.
macOS and Linux are the initial target platforms.
