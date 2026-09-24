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
push the matching tag (`v0.2.0`). The Desktop Packages workflow attaches a
drag-and-drop `.dmg` containing the macOS `.app`, a zipped `.app`, and an
`.AppImage` for each Linux architecture. Raw archives, checksums, and a shell
installer are also published. The v0.1.0 release currently has the `.app.zip` and
AppImage assets; its DMG build is pending GitHub Actions billing access. On macOS,
unzip the `.app.zip` and move SQL Manager to Applications. On Linux, mark the
`.AppImage` executable once (`chmod +x SQL-Manager-linux-x86_64-v0.1.0.AppImage`)
and launch it. The current macOS `.app.zip` is ad-hoc signed but not notarized,
so Gatekeeper may still ask you to approve it on first open. See
[`docs/macos-signing.md`](docs/macos-signing.md) for the workaround and the
Developer ID/notarization setup needed to remove that warning.
