# Changelog

## Unreleased

- Pin LightCraft `e9c29e2c` (catalog format 12, was 9). A library opened by a current LightCraft is upgraded to format 12, which the format 9 importer refused on replay.
- Map digiKam's camera make to LightCraft's `make` field and the EXIF Flash value to `flash` (fired or not; values outside the EXIF range stay unset).
- Read each XMP sidecar once on case-insensitive volumes (APFS, NTFS): `x.xmp`, `x.XMP` and `x.Xmp` were the same file read three times, which tripled packets in bundles and raised a false `multiple-sidecars` notice for nearly every photo with a sidecar.
- Adapter `lightcraft-0.20-v3`.

## 0.1.0 — 2026-10-20

- Inspect digiKam core SQLite catalogs and explicit photo-root mappings without modifying sources.
- Preserve complete core metadata and adjacent XMP in versioned, checksummed migration bundles.
- Create native LightCraft libraries that reference originals, with missing-file placeholders, metadata, keywords, named face regions, approximate XMP adjustments and image stacks.
- Replay upgraded mappings with stable source identities and independent protection of changed LightCraft field groups.
- Recover interrupted imports using native commit markers and durable checkpoints, including photos deleted after an interrupted run.
- Reconcile uncatalogued filesystem files separately, excluding trash and resource forks.
- Reject unsupported source structures, live/changing WALs, corrupt bundles, unrelated destinations and unsafe report outputs.
- Preserve fractional-second exposure times without rounding them to incorrect reciprocals.
- Include 26 automated tests covering persistence, replay, input validation, sidecar ownership, EXIF orientation and source preservation.

Known limits: video records are catalogued without playback; HEIF viewing requires a LightCraft build with HEIF enabled; foreign develop rendering is approximate. Full source metadata remains available for later adapters. See README for the mapping and archive scope.
