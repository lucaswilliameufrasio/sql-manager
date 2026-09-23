# SQL Manager

A lightweight native SQL client built with Rust and egui. PostgreSQL is the first
target. An engine adapter interface keeps connection testing and database sessions
behind a driver boundary for future engines.

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

## CI and releases

The CI workflow runs formatting, Clippy, tests, and a release build on Linux and
macOS. To prepare a release, run **Actions → Prepare Release** on `main` with a
semantic version such as `0.2.0`, review and merge the generated release PR, then
push the matching tag (`v0.2.0`). The tagged release workflow publishes archives
for Apple Silicon and Intel macOS, plus x86_64 and ARM64 Linux, checksums, and a
shell installer.
