# Changelog

All notable changes to SQL Manager are documented here.

## [0.2.0] - 2026-09-24

### Bug Fixes

- Label architecture-specific desktop packages
- Ad-hoc sign macOS app bundles
- Clear connection URLs after parsing

### Documentation

- Clarify desktop package release status

### Features

- Package macOS app and Linux AppImage
- Support PostgreSQL connection URLs
- Add SQL Manager branding assets
## [0.1.0] - 2026-09-23

### Bug Fixes

- Harden encrypted backup and SSH config
- Clear database passwords after use

### CI / Build

- Add macOS and Linux release workflows

### Chores

- Add README
- Bootstrap Rust egui desktop app
- Prepare for v0.1.0

### Features

- Add postgres connection profiles
- Add encrypted connection backups
- Add postgres sql workspace
- Add visual table row management
- Add visual schema operations
- Add OpenSSH connection tunnels

### Refactoring

- Add database engine adapter boundary
