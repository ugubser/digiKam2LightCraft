# digiKam2LightCraft

A standalone, pure-Rust migration tool for moving a digiKam library into LightCraft. It references originals in place, preserves source metadata for future adapters, and protects subsequent LightCraft edits when an import is replayed.

Version 0.1 supports digiKam's core SQLite catalog, validated against schema 17, and LightCraft catalog format 9. LightCraft's APIs are pinned to commit `d999e382bafbb5d5c54482fdb1d11f6a63bfdf69` (v0.9.1). No LightCraft source checkout or digiKam installation is required to build. Rendering uses the installed LightCraft application; this tool does not decode every original during migration.

## Build

Install Rust 1.95 or later, then:

```sh
cargo build --release --locked
./target/release/digikam2lightcraft --help
```

Initial builds fetch the pinned LightCraft dependencies. Runtime has no network requirement and uses no C/C++ SQLite library. Python 3 is only needed to generate synthetic databases for the test suite.

## Migration workflow

Use a stable backup of `digikam4.db`. A nonempty SQLite WAL is rejected: take a consistent SQLite backup first rather than separating a live database from its WAL. Nothing in the source database, photo directories or sidecars is written.

Report outputs selected with `--output` require a new `.json` path outside source data and migration bundles. Existing files are never replaced by these commands.

Map every active digiKam album root explicitly. The numeric root id appears in the `AlbumRoots` table. Mappings can be repeated for multiple roots.

```sh
# Database and path inventory; emits JSON.
./target/release/digikam2lightcraft inspect \
  --database /backups/digiKam/digikam4.db \
  --root '1=/photos' --output inspection.json

# Preserve the core database and adjacent XMP packets in an immutable bundle.
./target/release/digikam2lightcraft extract \
  --database /backups/digiKam/digikam4.db \
  --root '1=/photos' --bundle migration-bundle

# First try a separate pilot library. Raise/remove the limit to resume.
./target/release/digikam2lightcraft import \
  --bundle migration-bundle --library LightCraft-pilot --limit 1000

# Build a full, separate library. Do not select an existing personal library.
./target/release/digikam2lightcraft import \
  --bundle migration-bundle --library LightCraft-migrated

./target/release/digikam2lightcraft verify \
  --bundle migration-bundle --library LightCraft-migrated

# Account separately for files that are absent from active database records.
./target/release/digikam2lightcraft inventory \
  --bundle migration-bundle --output filesystem-inventory.json
```

Open `LightCraft-migrated` using LightCraft's **File → Open Library**. Close that library before running another import: the importer uses LightCraft's exclusive catalog lock. Your existing LightCraft library remains separate.

`extract --sample 1000` makes a smaller pilot bundle, including each available format before distributing samples across the catalog. A sample is not a complete migration archive. `--no-sidecars` explicitly omits sidecars and their edits; it is intended for database-only diagnostics.

For a bundle moved to another machine, override its original locations at import time:

```sh
./target/release/digikam2lightcraft import \
  --bundle migration-bundle --library LightCraft-migrated \
  --root '1=/new/photo/location'
```

The paths change; the original archived XMP bytes do not. Original photo files must be backed up/transferred separately. The migration bundle contains metadata, not the image/video bytes.

## Mappings and current limitations

Run `digikam2lightcraft capabilities` for a machine-readable summary.

| Source | Native result / policy |
| --- | --- |
| Active images | Native catalog records referencing existing paths; no copying, renaming or byte hashing of originals |
| Inactive/removed records | Remain in the archived database; never automatically resurrected |
| Missing originals | Retain metadata and a record pointing at the missing path |
| Physical albums | Existing folder hierarchy appears through LightCraft's folder browser; no duplicate album per directory |
| Ratings | 0–5 stars; digiKam database `-1` means **unrated**, while XMP `-1` means rejected |
| Keywords | Hierarchical paths; internal bookkeeping and provisional person tags are excluded |
| Pick labels | Accepted → Pick, Rejected → Reject, Pending → `digiKam\|Pending` keyword |
| Colour labels | Direct native colours; Magenta → Purple, other unsupported colours → descriptive keywords |
| Text, camera/lens, exposure, capture dates, GPS | Supported native fields; full source values remain archived |
| GPS altitude/accuracy, IPTC contacts, extra comments/languages | Archived where no native field exists |
| Face regions | Named annotations by default; `--faces all` includes automatic unnamed detections, `--faces none` omits native annotations |
| RAW/JPEG sidecars | Exact full-name packet first; shared stem packet belongs to the raw, then the first filename; other packets remain archived |
| Develop adjustments | LightCraft's existing XMP mapper: supported adjustments are editable, foreign rendering is approximate; unsupported fields are reported |
| Group relationships | Add native stacks when the leader and members are available; existing stacks are not rearranged |
| Video | Native records and duration; LightCraft playback, trimming and export remain unsupported |
| HEIF | Native records; viewing requires a LightCraft build with its `heif` feature |
| Saved searches, similarity queries, unused tag definitions, album-specific metadata, manual order, edit history and unknown fields | Preserved in the core database snapshot for future adapters; not translated into native equivalents in v0.1 |

Database ratings and nonempty descriptive fields take precedence by default. XMP supplements missing fields, keywords and face annotations, and supplies develop adjustments. Use `--metadata-source sidecar` to prefer supported sidecar values instead. Conflicts are reported; additional sidecars remain available in the bundle. Neither choice writes back to XMP. Automatic sidecar writes are suppressed during migration; existing library preferences are retained.

Database face rectangles are normalized against the oriented frame. MWG regions with explicit rotations use LightCraft's parser. Without an explicit MWG rotation, a sidecar's TIFF orientation is applied to its sensor-frame rectangles. This is checked against paired database/XMP annotations, with synthetic coverage for all eight EXIF orientations. Original coordinate values remain available for correcting unusual source conventions in a later adapter.

The bundle preserves the **core database and adjacent XMP packets**. It does not include digiKam's thumbnail/similarity caches, recognition database, external preset/profile files or XMP embedded inside media. Keep the original backup for these. This is a one-way migration, not bidirectional synchronization. Uncatalogued filesystem files are reported separately and not automatically added.

## Replay as LightCraft improves

Keep the bundle and the destination's `Interop/digikam2lightcraft` directory. After upgrading the adapter, run `import` again against the same bundle and library. For example, `--faces all` can add previously archived automatic regions without replacing a changed rating or develop adjustment.

Stable identities are `(source database UUID, digiKam image id)`. Replay is intended for the same immutable bundle. A refreshed source snapshot must retain the same catalog lineage and stable image ids; regenerated catalogs or reused ids need a fresh destination.

The importer records fingerprints for independent groups: rating, flag, label, captured time, duration, develop, source path, filename, technical information, descriptive metadata, keywords and regions. On replay:

- A value still equal to its last imported value can receive a new mapping.
- A value changed in LightCraft is preserved and reported.
- A photo removed from the target is not resurrected.
- Existing stacks are left intact; groups made entirely of previously imported photos are not recreated after a user unstacks them.

Descriptive metadata is one group: changing a title can hold back other descriptive-field refreshes. Keywords and regions are separate. Newly introduced groups with no previous baseline are conservatively protected; a future adapter must define a safe default or an explicit adoption policy.

Each batch stages a migration checkpoint before the native catalog commit, then publishes it after the catalog is durable. A restart reconciles an unfinished checkpoint against the saved catalog before continuing. Repeated imports do not duplicate photos. A full source UUID mismatch or an unrelated destination is rejected.

Detailed per-photo notices and summaries are written under `Interop/digikam2lightcraft/reports`. `verify` reports imported identities, missing originals, absent target records and field groups changed after migration. It does not claim pixel-level rendering equivalence.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for bundle/state formats and how to extend an adapter, and [THIRD_PARTY.md](THIRD_PARTY.md) for the SQLite reader's provenance. Real catalogs, migration bundles, screenshots and reports belong in ignored `work/` or `artifacts/` directories; never commit personal metadata or media.

## License

MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and [THIRD_PARTY.md](THIRD_PARTY.md).
