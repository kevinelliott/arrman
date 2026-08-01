## What changed

Describe the behavior and why it belongs in Arrman.

## Upstream evidence

Link primary documentation for service images, ports, dependencies, health, and APIs.

## Safety and compatibility

- [ ] No credentials, private addresses, personal paths, or real configuration were committed.
- [ ] Existing Compose and application fields are preserved.
- [ ] Enable, disable, dependency, port-change, and integration effects are explicit.
- [ ] Backup/restore compatibility is unchanged or migration impact is documented.
- [ ] TUI and web behavior remain consistent.

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `node --check src/web_assets/app.js`
- [ ] `node scripts/verify-docs.mjs`
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `docker build --build-arg VERSION=contributor --tag arrman:contributor .`
