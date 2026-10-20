# Architecture and extension contract

## Separation of responsibilities

1. `source.rs` reads the public SQLite file format using the permissively licensed LightCraft page reader. It interprets digiKam's tables from their column declarations, validates required structures, and rejects an active WAL. It never uses digiKam implementation code or a SQLite runtime.
2. `bundle.rs` captures a stable core database snapshot and streams target-independent records. `model.rs` defines the versioned interchange schema.
3. `adapter.rs` maps one source record to native LightCraft values. `capabilities` exposes its id, native mappings, approximations and deferred data. The adapter is the place to extend feature support.
4. `migrate.rs` owns destination identity, safe replay, native catalog operations and durable migration checkpoints. Translators do not write files themselves.
5. `inventory.rs` independently reconciles disk files with active source records. It excludes trash/resource forks and never turns a directory scan into an implicit import.

The LightCraft dependency is pinned by revision and `Cargo.lock`. Writes use public `Session`, `Op` and journal APIs: no handcrafted native snapshot, checksum, or log format. A library is opened without demo seeding or pixel probing; actual rendering belongs to LightCraft. The SQLite reader is the only vendored code and its origin/licence are recorded.

## Bundle version 1

A completed bundle has:

- `manifest.json`: format/version, source database UUID/schema/hash, explicit photo-root mappings, created time, table counts, record-stream hash, selection size, sampling and sidecar inclusion flags.
- `digikam4.db`: the complete core database snapshot. This is the recovery source for unknown tables, columns and source features that the current record model does not expose.
- `records.jsonl.gz`: one `Record` per active image, with source ids, source rows grouped by table, tag paths/properties, group leader, original path/presence and original adjacent XMP packets. UTF-8 packets are retained verbatim; other encodings use base64. Each packet has a hash. Applicability records which shared sidecar belongs to a different source file.

The source's private data is deliberately absent from the repository and test fixtures. The archive is not a photo backup. Thumbnail/similarity caches and face-training database bytes are outside its scope; retain the original digiKam backup as well.

Extraction writes into an exclusive sibling staging directory and publishes the bundle only after data is flushed and the manifest is durable. Failed staging directories remain for diagnosis. An existing completed bundle is never overwritten. Input hashes and the complete bounded record stream are checked before opening a destination for mutation. SHA-256 detects accidental damage; these checks are not an authenticity signature.

Limits are explicit: core database ≤1 GiB, SQLite table limits inherited from the documented reader, at most one million records, XMP packet ≤16 MiB, record line ≤64 MiB, manifest ≤4 MiB, tag ancestry ≤128 levels, filesystem walk ≤64 levels/100,000 directories. Oversized input fails with an actionable error rather than silently dropping metadata.

## Destination state version 1

`Interop/digikam2lightcraft/owner.json` ties the managed destination to a source UUID. Nonempty unmanaged libraries cannot be adopted by v0.1.

`state/batch-NNNNNNNNNNNN.json.gz` is an immutable ordered checkpoint stream. Each entry maps a source image id to its native target id and baseline SHA-256 hashes of the field groups last imported. A checkpoint also carries the previous entry, enabling recovery to distinguish a commit that landed from one that did not.

Before native catalog operations are committed, `state/pending.json.gz` is published atomically. The same native operation batch includes a commit marker, stored through `Op::SetFolderRecord` under the library's Interop/state/commits namespace. A folder record with no associated photographs is invisible in LightCraft's folder tree. This durable marker distinguishes an import that committed and was subsequently edited/deleted from one that never committed; catalog sequence numbers alone cannot make that distinction. After LightCraft's journal acknowledges durability, pending is atomically renamed to its final batch name. Recovery uses the native marker before publishing the reconciled checkpoint; historical pre-marker pending files retain a conservative fingerprint fallback. Corrupt or discontinuous state is an error, never a reason to guess identities or add duplicates.

The catalog's exclusive lock protects both the native catalog and migration state. Reports are per-run compressed JSON lines plus a summary. Automatic XMP writes are suppressed; the importer has no sidecar-write path.

## Adding support

1. Pin the LightCraft revision that provides the new native capability. Check its catalog compatibility and public API changes.
2. Add/extend a mapping in `adapter.rs`, and bump `model::ADAPTER` when behavior changes. Keep the neutral bundle readable independently of adapter changes.
3. Recover previously deferred source values from existing record rows. If a field exists only in the SQLite snapshot, extend record extraction and provide a bundle-upgrade operation or re-extraction from the archived database. Never require the original live digiKam catalog just to recover already archived core fields.
4. Route writes through the existing guarded field groups. A new group needs a conservative adoption/default policy; missing historical baselines do not authorize overwriting a current target value.
5. Add a synthetic regression demonstrating the old omission, native save/reopen, a no-op second replay, preservation of a changed LightCraft value, and interrupted-checkpoint recovery.
6. Update `capabilities`, mapping documentation and release notes. Run format, clippy and tests before committing.

Unknown fields remain recoverable in the original core snapshot even when current translation is incomplete. Source hashes must never be mistaken for LightCraft content hashes: algorithms and semantics differ. Explicit source root overrides allow a bundle to be replayed against another filesystem location without editing its archived packets.
