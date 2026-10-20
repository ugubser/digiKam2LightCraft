# Project instructions

- Preserve the source. Never write to digiKam databases, source photo directories or existing XMP packets. Work against backups and separate managed LightCraft libraries.
- Keep extraction/bundle formats separate from target mapping. Future adapters must be replayable while preserving LightCraft edits, source identities and deferred metadata.
- Product code is pure Rust, no unsafe code, no C/C++ runtime dependencies and no production panics. Python is test-fixture tooling only. Use bounded parsing, checked arithmetic and actionable errors for hostile/corrupt input.
- Never read or copy GPL/LGPL/AGPL implementation code or proprietary application assets. Preserve third-party notices. Source metadata and public format/schema documentation are allowed.
- Never handcraft LightCraft native snapshots or journal records. Use the pinned public catalog/engine APIs and native exclusive lock. Add tests for recovery and no-op replay when changing persistence or mappings.
- Real catalogs, bundles, media, personal paths/names and reports stay in ignored `work/` or `artifacts/`. Commit only original synthetic fixture generators.
- Before each commit: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`. Keep commits small and green. Do not commit a local Cargo dependency override.
- Do not contact upstream maintainers or publish personal metadata. The configured Git remote is for this importer project only.
