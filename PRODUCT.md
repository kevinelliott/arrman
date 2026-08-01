# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Product Name

Arrman

## Stack

Standalone Rust application using Ratatui/Crossterm for the terminal and Axum
with a dependency-free browser client for the optional web control room. Docker
Engine and Docker Compose remain the runtime authorities. One Linux binary owns
both interactive surfaces and does not require Python or a separate web runtime.

## Users

The primary user is the home media-server administrator operating the server
locally or over SSH. They need to understand and change a multi-service Docker
Compose stack without memorizing container names, dependency order, ports, log
commands, or integration-specific APIs.

## Product Purpose

Provide one trustworthy control plane for the media stack: reconcile declared
Compose intent with live Docker state, explain health and dependencies, safely
enable or disable services, edit ports, inspect problems, and maintain supported
service integrations. The terminal and authenticated web surfaces must remain in
sync while both are open. Success means common operations are fast, observable,
portable, and recoverable without dropping to ad hoc shell commands.

## Positioning

The manager treats Compose configuration, Docker runtime state, persistent
enablement intent, dependency health, logs, and application integrations as one
operational model. Actions update the full affected dependency graph and report
their progress instead of acting as isolated container start or stop shortcuts.

## Operating Context

- Runs directly on the Alpine Linux media server, commonly inside an SSH terminal.
- Optionally serves a loopback-only browser control room reached through an SSH
  tunnel, or runs web-only under a process supervisor.
- Manages `/root/docker-compose.yml`, Docker containers, bind-mounted service
  configuration under `/nfs`, Docker named volumes, and private control files.
- Must remain usable in narrow terminal windows as well as wide desktop terminals.
- Long Docker pulls, health waits, log scans, and integration calls are normal.
- The same data must be available non-interactively through `--json`.
- Setup bundles live on NAS-backed storage; the root filesystem is not suitable
  for multi-gigabyte retained configuration snapshots.

## Capabilities and Constraints

- Show host IP, CPU, load, memory, disk, uptime, services, URLs, health, issues,
  dependencies, and integration state.
- Support fast keyboard navigation, filtering, service inspection, log scanning,
  confirmed fixes, enable/disable, restart, and validated port changes.
- Enabling resolves dependencies in start order. Disabling resolves dependents
  in stop order and includes service-owned dependencies.
- Disabled state is durable and must not be confused with merely stopping a
  container. Disabled containers use Docker restart policy `no`.
- Port edits and integration writes create recoverable backups and validate the
  resulting Compose model before recreating a service.
- The TUI and web interface share one operation permit, activity stream, state
  model, and recurring-backup scheduler. Cross-process mutation is also locked.
- The web interface supports live management, diagnostics, integration
  reconciliation, and guarded setup export/import from a local file or HTTPS URL.
- Versioned setup bundles include control files, configuration mounts, and named
  volumes, including service ownership metadata needed for safe NFS restoration,
  while explicitly excluding media, downloads, caches, logs, devices, generated
  assets, the Docker socket, and per-volume transient or unsafe-symlink paths.
- Restore always inspects before mutation, creates a pre-restore bundle, stops the
  stack, validates staged paths and Compose, and rolls back on failure.
- Commands use explicit argument vectors; user-controlled service names and ports
  are validated and never interpolated through a shell.
- Existing credentials are read only where required for local integration work.
  Secret values are never shown in the interface, JSON, logs, or command output.
- Docker Compose remains the source of truth for service definitions.

## Verification Sources

- Deterministic demo snapshots exercise the responsive terminal and web models
  without requiring a private media stack.
- Unit tests cover dependency ordering, durable disablement, safe port edits,
  integration redaction, diagnostics, authenticated web routes, and guarded
  backup/restore behavior.
- Docker Compose and the Docker Engine remain the runtime authorities during
  live installation verification.

## Product Principles

- State must be legible before an action is offered.
- Every long-running action stays visible and never blocks navigation rendering.
- Every connected surface converges on the same operation and current state.
- Destructive or graph-wide changes show their exact scope before confirmation.
- Errors name the failed operation, preserve its output, and suggest recovery.
- Automation output is stable, complete, and free of terminal decoration.

## Accessibility & Inclusion

All operations are keyboard-accessible. Color reinforces but never replaces
text labels or status symbols. Terminal and browser layouts adapt structurally to
constrained viewports, focus is visible, controls retain usable touch targets,
and the JSON mode provides a non-visual automation path.
