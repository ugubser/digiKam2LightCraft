# Third-party code

`src/sqlite.rs` is the read-only, pure-Rust SQLite page reader from [LightCraft](https://github.com/storytold/lightcraft), file `crates/engine/src/lightroom_sqlite.rs`, at commit `d999e382bafbb5d5c54482fdb1d11f6a63bfdf69`.

Copyright (c) 2026 ArtCraft Team. Licensed MIT OR Apache-2.0; full licence texts and notices are retained in `LICENSE-MIT` and `LICENSE-APACHE`. The reader was developed from SQLite's [public file-format specification](https://www.sqlite.org/fileformat.html). It is reused with formatting changes only; the surrounding digiKam schema adapter is original code based on database structure and metadata observations.

The native catalog and XMP adapter use the same permissively licensed LightCraft project as a pinned Git dependency. Other dependencies and exact versions are recorded in `Cargo.lock`; their upstream packages carry their respective licences. No digiKam, LibRaw, rawspeed, rawloader, rawler, darktable, RawTherapee, or other GPL/LGPL/AGPL implementation code is included.

All media used in automated tests is generated procedurally by `tests/make_fixture.py`. No photo, database, sidecar, profile, icon, font or application asset from a user's collection is distributed here.
