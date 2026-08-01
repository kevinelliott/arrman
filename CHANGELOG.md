# Changelog

All notable changes to Arrman are documented here.

## [1.0.1] - 2026-07-31

### Security

- Upgraded Ratatui to 0.30 and aligned Crossterm to 0.29 so Ratatui's transitive
  `lru` dependency is on the patched release line, addressing
  `RUSTSEC-2026-0002` without bundling unrelated major dependency updates.

### Changed

- Removed a styling-trait import made redundant by Ratatui 0.30.

## [1.0.0] - 2026-07-31

### Added

- Rust operations core that reconciles Docker Compose intent with live Docker
  state, durable service enablement, dependency ordering, ports, and health.
- Responsive Ratatui terminal interface with fast cached navigation, diagnostics,
  integration review, recovery controls, and a persistent activity channel.
- Authenticated Axum web control room that can run alongside the TUI and reflects
  shared realtime state and operations.
- Log issue detection with evidence, manual guidance, and confirmed bounded
  repairs.
- Dynamic SABnzbd, Prowlarr, Scraparr, and Tracearr integration planning.
- Versioned recurring setup bundles, guarded local/HTTPS import, checksums,
  ownership metadata, exclusions, pre-restore backup, and rollback.
- First-class metadata for 34 application and dependency services.
- Static Linux release builds, multi-architecture GHCR images, Docker Compose
  deployment support, CI, release automation, provenance, and SBOM generation.

[1.0.1]: https://github.com/kevinelliott/arrman/releases/tag/v1.0.1
[1.0.0]: https://github.com/kevinelliott/arrman/releases/tag/v1.0.0
