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
1,000-row result cap, and schema/table browsing. The remaining milestones include
SSH tunneling and visual data/schema management.
macOS and Linux are the initial target platforms.
