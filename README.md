# SQL Manager

A lightweight native SQL client built with Rust and egui. PostgreSQL is the first
target, with an architecture planned to support additional database engines.

## Development

Install the Rust toolchain, then run the desktop application with:

```sh
cargo run
```

The current bootstrap opens the native egui window. The initial milestones are
PostgreSQL connections (including SSL and SSH tunneling), encrypted connection
backup and restore, schema browsing, SQL execution, and visual data/schema
management. macOS and Linux are the initial target platforms.
