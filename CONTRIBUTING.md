# Contributing to Arrman

Thanks for helping make media-stack operations less fragile. Arrman favors
small, evidence-backed adapters over broad assumptions: Docker Compose remains
the source of truth, existing application configuration is preserved, and
secrets never enter user-visible state.

## Development setup

Install Rust 1.88 or newer, Node.js, Docker Engine, and Docker Compose v2. The
core test suite does not require a live media stack.

```sh
git clone https://github.com/kevinelliott/arrman.git
cd arrman
cargo test --locked
cargo run --locked -- --demo --snapshot --width 136 --height 42
```

Before opening a pull request, run the complete local gate:

```sh
cargo fmt --all -- --check
node --check src/web_assets/app.js
node scripts/verify-docs.mjs
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --release --locked
docker build --build-arg VERSION=contributor --tag arrman:contributor .
```

## Adding service support

Unknown Compose services already appear in Arrman with generic metadata. Add
only the layers the service genuinely needs:

1. Verify the upstream repository, maintained image, default internal port,
   dependencies, health endpoint, persistent configuration paths, and runtime
   UID/GID from primary project documentation.
2. Add the Compose service name and purpose to `PURPOSES` in `src/compose.rs`.
3. If a dependency exists only for this application, add it to
   `EXCLUSIVE_DEPENDENCIES`. Never mark a shared database, cache, proxy, or
   downloader as exclusive.
4. Add an integration adapter in `src/integrations.rs` only when the upstream API
   supports a safe read/compare/create-or-update loop. Preserve unknown fields,
   avoid duplicate records, and make repeated application a no-op.
5. Add bounded log signatures and repair guidance in `src/diagnostics.rs`. An
   automatic fix must be reversible or narrowly scoped and must still require
   confirmation.
6. Add fixtures and tests for dependency order, durable disablement, port
   changes, integration idempotency, malformed responses, and redaction.
7. Add the exact service name and upstream link to
   `docs/SUPPORTED_SERVICES.md`; CI verifies that it matches the source catalog.

### Sanitized Compose fixture

Never submit a real home-server Compose file. A fixture should contain only what
the test needs and must replace:

- hostnames and private addresses with documentation examples;
- API keys, passwords, tokens, cookies, and webhook URLs with inert placeholders;
- personal mount paths with temporary fixture paths;
- provider account identifiers and private indexer configuration with fake data.

Prefer a focused fixture over a full application stack. Include an upstream
source link in the test or pull-request description for non-obvious health and
dependency behavior.

### Integration adapter contract

An adapter should follow this sequence:

1. Discover enabled and running prerequisites from the normalized runtime model.
2. Read current application configuration through the documented local API.
3. Compare semantically, including masked-secret behavior where the API hides an
   existing key.
4. Produce a redacted plan that names the target, effect, and manual fallback.
5. Apply only after confirmation, retain unknown fields, and verify the result.
6. Report a stable success, blocked, or actionable failure state.

Do not place secret values in `Debug`, `Display`, serialized models, command
arguments visible in activity, URLs, snapshots, tests, or fixtures.

## UI contributions

The TUI and web UI represent the same control plane. A new action must have the
same scope, confirmation, busy state, completion/error reporting, and refresh
behavior in both surfaces. Keep keyboard operation complete and ensure color is
never the only state signal.

For visual changes, verify wide, medium, and narrow TUI snapshots and desktop,
tablet, and phone web layouts. Preserve the existing design tokens and square
engineering-watch geometry unless a deliberate design change is documented.

## Pull requests

Keep commits coherent and explain:

- the service or behavior being added;
- the upstream evidence used;
- dependency and health-check assumptions;
- how enable, disable, and port changes affect integrations;
- what is automatic versus manual;
- test and container-build results;
- any migration or backup compatibility impact.

By contributing, you agree that your changes are licensed under the MIT License.
