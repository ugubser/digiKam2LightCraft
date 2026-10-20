use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use digikam2lightcraft::{
    adapter::{self, Faces, MetadataSource, Options},
    bundle, io, migrate,
    model::{Record, RootMapping},
    source::Source,
};
use lightcraft_engine::{
    Session,
    catalog::{Flag, MediaKind, PhotoId},
};

fn fixture(path: &Path) -> Result<()> {
    let status = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/make_fixture.py"))
        .arg(path)
        .status()?;
    ensure!(status.success(), "fixture generation failed");
    Ok(())
}
fn source(path: &Path) -> Result<Source> {
    Source::open(
        &path.join("database/digikam4.db"),
        vec![RootMapping {
            id: 1,
            path: path.join("photos"),
        }],
    )
}

#[test]
fn source_bundle_native_catalog_and_safe_replay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let source = source(temp.path())?;
    let inspect = bundle::inspect(&source)?;
    assert_eq!(inspect["activeRecords"], 7);
    assert_eq!(inspect["present"], 6);
    let path = temp.path().join("bundle");
    let manifest = bundle::extract(&source, &path, None, true)?;
    assert_eq!(manifest.records, 7);
    assert_eq!(io::hash_file(&path.join("digikam4.db"))?, source.sha256);
    let library = temp.path().join("library");
    let report = migrate::import(&path, &library, Options::default(), None)?;
    assert_eq!(report.added, 7);
    assert_eq!(report.groups_added, 1);
    assert_eq!(report.missing_originals, 1);
    let mut session = Session::new();
    session.open_library(&library, false)?;
    assert_eq!(session.catalog.len(), 7);
    assert_eq!(session.catalog.stacks().count(), 1);
    let one = session
        .catalog
        .photo(PhotoId(1))
        .context("first photo missing")?;
    assert_eq!(one.rating, 2);
    assert_eq!(one.width, 300);
    assert_eq!(one.height, 400);
    assert_eq!(one.meta.caption, "database caption");
    assert_eq!(one.meta.title, "database title");
    assert_eq!(one.meta.gps, Some((47.0, 8.0)));
    assert!(one.content_hash.is_none());
    assert!(one.meta.keywords.contains(&"Travel|日本".to_string()));
    assert!(one.meta.keywords.contains(&"digiKam|Pending".to_string()));
    assert_eq!(
        one.meta.regions.len(),
        1,
        "database and oriented XMP face should deduplicate"
    );
    assert!((one.meta.regions.first().context("face missing")?.rect.x0 - 0.1).abs() < 1e-6);
    assert!(
        (one.develop.light.exposure - 1.0).abs() < 1e-6,
        "full-name XMP should win over stem"
    );
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(2))
            .context("second photo")?
            .flag,
        Flag::None
    );
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(2))
            .context("second photo")?
            .rating,
        0,
        "database -1 means unrated, not rejected"
    );
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(3))
            .context("video missing")?
            .kind,
        MediaKind::Video
    );
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(3))
            .context("video missing")?
            .duration,
        Some(12.345)
    );
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(7))
            .context("HEIF missing")?
            .format,
        "HEIF"
    );
    session.execute("photo.rate", &serde_json::json!({"ids":[1],"rating":5}))?;
    session.execute(
        "develop.set",
        &serde_json::json!({"ids":[1],"control":"light.exposure","value":0.7}),
    )?;
    session.close_library()?;
    drop(session);
    let report = migrate::import(
        &path,
        &library,
        Options {
            faces: Faces::All,
            ..Default::default()
        },
        None,
    )?;
    assert_eq!(report.added, 0);
    assert_eq!(
        report.updated, 1,
        "automatic region support should be replayed independently"
    );
    assert!(report.preserved_user_fields >= 2);
    let mut session = Session::new();
    session.open_library(&library, false)?;
    let one = session.catalog.photo(PhotoId(1)).context("first photo")?;
    assert_eq!(one.rating, 5);
    assert!((one.develop.light.exposure - 0.7).abs() < 1e-6);
    assert_eq!(
        session
            .catalog
            .photo(PhotoId(2))
            .context("second photo")?
            .meta
            .regions
            .len(),
        1
    );
    assert_eq!(
        session.catalog.stacks().count(),
        1,
        "replay must not duplicate stacks"
    );
    session.close_library()?;
    drop(session);
    let report = migrate::import(
        &path,
        &library,
        Options {
            faces: Faces::All,
            ..Default::default()
        },
        None,
    )?;
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    let verify = migrate::verify(&path, &library)?;
    assert_eq!(verify["imported"], 7);
    assert_eq!(verify["changedLightcraftFields"], 2);
    assert_eq!(
        io::hash_file(&temp.path().join("database/digikam4.db"))?,
        source.sha256,
        "source must remain unchanged"
    );
    Ok(())
}

#[test]
fn corrupt_archive_is_rejected_before_destination_creation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let source = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&source, &path, None, true)?;
    std::fs::write(path.join("records.jsonl.gz"), b"damaged")?;
    let library = temp.path().join("library");
    assert!(migrate::import(&path, &library, Options::default(), None).is_err());

    assert!(!library.exists());
    Ok(())
}

#[test]
fn unrelated_destination_and_wal_are_rejected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let source = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&source, &path, None, true)?;
    let library = temp.path().join("library");
    std::fs::create_dir(&library)?;
    std::fs::write(library.join("keep.txt"), "keep")?;
    assert!(migrate::import(&path, &library, Options::default(), None).is_err());

    assert_eq!(std::fs::read_to_string(library.join("keep.txt"))?, "keep");
    std::fs::write(temp.path().join("database/digikam4.db-wal"), b"live WAL")?;
    assert!(
        Source::open(
            &temp.path().join("database/digikam4.db"),
            vec![RootMapping {
                id: 1,
                path: temp.path().join("photos")
            }]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn sidecar_precedence_and_all_orientation_geometry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let source = source(temp.path())?;
    let r = source.record(0, true)?;
    let (p, _) = adapter::map(
        &r,
        PhotoId(1),
        "2026-10-20T12:00:00",
        &Options {
            metadata_source: MetadataSource::Sidecar,
            ..Default::default()
        },
    )?;
    assert_eq!(p.rating, 4);
    assert_eq!(p.meta.caption, "sidecar caption");
    let expected = [
        [0.1, 0.2, 0.3, 0.4],
        [0.7, 0.2, 0.9, 0.4],
        [0.7, 0.6, 0.9, 0.8],
        [0.1, 0.6, 0.3, 0.8],
        [0.2, 0.1, 0.4, 0.3],
        [0.6, 0.1, 0.8, 0.3],
        [0.6, 0.7, 0.8, 0.9],
        [0.2, 0.7, 0.4, 0.9],
    ];
    for (i, e) in expected.iter().enumerate() {
        let actual = adapter::orient_rect([0.1, 0.2, 0.3, 0.4], i as i64 + 1);
        assert!(actual.iter().zip(e).all(|(a, b)| (a - b).abs() < 1e-9));
    }
    assert!(
        adapter::database_rect("<rect x=\"NaN\" y=\"0\" width=\"1\" height=\"1\"/>", 2, 2)
            .is_none()
    );
    assert!(
        adapter::database_rect("<rect x=\"0\" y=\"0\" width=\"1\" height=\"1\"/>", 0, 2).is_none()
    );
    Ok(())
}

#[test]
fn interrupted_commit_recovery_and_partial_import_resume() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let source = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&source, &path, None, true)?;
    let library = temp.path().join("library");
    let r = migrate::import(&path, &library, Options::default(), Some(3))?;
    assert_eq!(r.added, 3);
    // Catalog commit is durable; index publication was interrupted. Recover the pending
    // checkpoint instead of duplicating the already imported three records.
    let state = library.join("Interop/digikam2lightcraft/state");
    std::fs::rename(
        state.join("batch-000000000001.json.gz"),
        state.join("pending.json.gz"),
    )?;
    let report = migrate::import(&path, &library, Options::default(), None)?;
    assert_eq!(report.added, 4);
    assert_eq!(report.unchanged, 3);
    let result = migrate::verify(&path, &library)?;
    assert_eq!(result["imported"], 7);
    assert_eq!(result["catalogPhotos"], 7);
    Ok(())
}

#[test]
fn unknown_metadata_and_binary_packet_survive_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    std::fs::write(
        temp.path().join("photos/two.PNG.xmp"),
        [0xff, 0xfe, 0, 1, 2, 3],
    )?;
    let source = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&source, &path, None, true)?;
    let (_, manifest) = bundle::open(&path)?;
    assert_eq!(manifest.table_counts.get("FutureMetadata"), Some(&1));
    let mut found = false;
    io::each_record::<Record>(&path.join("records.jsonl.gz"), |r| {
        if r.source_id == 2 {
            found = true;
            assert_eq!(
                adapter::packet_bytes(r.sidecars.first().context("packet absent")?)?,
                vec![0xff, 0xfe, 0, 1, 2, 3]
            );
        }
        Ok(())
    })?;
    assert!(found);
    Ok(())
}

fn sql(path: &Path, statements: &str) -> Result<()> {
    let status=Command::new("python3").arg("-c").arg("import sqlite3,sys;c=sqlite3.connect(sys.argv[1]);c.executescript(sys.argv[2]);c.commit()")
        .arg(path.join("database/digikam4.db")).arg(statements).status()?;
    ensure!(status.success(), "fixture update failed");
    Ok(())
}

#[test]
fn source_refresh_changes_only_untouched_fields_and_keeps_target_additions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let first = temp.path().join("first");
    bundle::extract(&src, &first, None, true)?;
    let library = temp.path().join("library");
    migrate::import(&first, &library, Options::default(), None)?;
    let mut s = Session::new();
    s.open_library(&library, false)?;
    s.execute("photo.rate", &serde_json::json!({"ids":[1],"rating":5}))?;
    s.commit(
        "Add unrelated photo",
        lightcraft_engine::catalog::Op::AddPhoto {
            photo: Box::new(lightcraft_engine::catalog::Photo::new(
                PhotoId(100),
                lightcraft_engine::catalog::Source::Demo { scene: 0 },
                "own.PNG",
                "PNG",
                2,
                2,
                "2026-10-20T12:00:00",
            )),
        },
    )?;
    s.close_library()?;
    drop(s);
    sql(
        temp.path(),
        "UPDATE ImageInformation SET rating=3 WHERE imageid IN (1,2);",
    )?;
    let src = source(temp.path())?;
    let second = temp.path().join("second");
    bundle::extract(&src, &second, None, true)?;
    let r = migrate::import(&second, &library, Options::default(), None)?;
    assert_eq!(r.added, 0);
    assert_eq!(r.updated, 1);
    let mut s = Session::new();
    s.open_library(&library, false)?;
    assert_eq!(s.catalog.len(), 8);
    assert_eq!(s.catalog.photo(PhotoId(1)).context("first")?.rating, 5);
    assert_eq!(s.catalog.photo(PhotoId(2)).context("second")?.rating, 3);
    assert!(s.catalog.photo(PhotoId(100)).is_some());
    Ok(())
}

#[test]
fn locked_library_and_removed_target_are_protected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&src, &path, None, true)?;
    let library = temp.path().join("library");
    migrate::import(&path, &library, Options::default(), None)?;
    let mut s = Session::new();
    s.open_library(&library, false)?;
    assert!(migrate::import(&path, &library, Options::default(), None).is_err());
    let stack = s.catalog.stacks().next().context("stack missing")?.id;
    s.commit(
        "Unstack",
        lightcraft_engine::catalog::Op::RemoveStack { id: stack },
    )?;

    s.commit(
        "Remove migrated photo",
        lightcraft_engine::catalog::Op::RemovePhoto { id: PhotoId(7) },
    )?;
    s.close_library()?;
    drop(s);
    let report = migrate::import(&path, &library, Options::default(), None)?;
    assert_eq!(report.added, 0);
    assert_eq!(report.removed_from_target, 1);
    let verify = migrate::verify(&path, &library)?;
    assert_eq!(verify["catalogPhotos"], 6);
    let mut s = Session::new();
    s.open_library(&library, false)?;
    assert_eq!(
        s.catalog.stacks().count(),
        0,
        "replay must preserve a user's unstack operation"
    );

    Ok(())
}

#[test]
fn shared_raw_jpeg_sidecar_is_archived_but_applied_to_owner_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    std::fs::copy(
        temp.path().join("photos/two.PNG"),
        temp.path().join("photos/one.JPG"),
    )?;
    sql(
        temp.path(),
        "INSERT INTO Images VALUES(9,1,'one.JPG',1,1,100,'h');INSERT INTO ImageInformation VALUES(9,0,NULL,400,300,1,'JPG');",
    )?;
    let src = source(temp.path())?;
    let idx = src
        .images
        .iter()
        .position(|r| digikam2lightcraft::model::integer(Some(r), "id") == Some(9))
        .context("JPEG")?;
    let record = src.record(idx, true)?;
    assert_eq!(record.sidecars.len(), 1);
    assert!(!record.sidecars.first().context("sidecar")?.applicable);
    let (p, notices) = adapter::map(
        &record,
        PhotoId(9),
        "2026-10-20T12:00:00",
        &Options::default(),
    )?;
    assert_eq!(p.develop.light.exposure, 0.0);
    assert!(notices.iter().any(|n| n.code == "shared-stem-sidecar"));
    Ok(())
}

#[test]
fn root_relocation_and_filesystem_inventory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&src, &path, None, true)?;
    std::fs::write(temp.path().join("photos/extra.JPG"), b"extra")?;
    std::fs::write(temp.path().join("photos/._extra.JPG"), b"resource fork")?;
    std::fs::create_dir_all(temp.path().join("photos/.dtrash/files"))?;
    std::fs::write(
        temp.path().join("photos/.dtrash/files/deleted.JPG"),
        b"trash",
    )?;
    let inventory = digikam2lightcraft::inventory::inspect(&path)?;
    assert_eq!(inventory["uncataloguedCount"], 1);
    assert_eq!(inventory["missingCount"], 1);
    let new_root = temp.path().join("relocated-photos");
    std::fs::rename(temp.path().join("photos"), &new_root)?;
    let library = temp.path().join("library");
    let options = Options {
        roots: vec![RootMapping {
            id: 1,
            path: new_root.clone(),
        }],
        ..Default::default()
    };
    let report = migrate::import(&path, &library, options, None)?;
    assert_eq!(report.added, 7);
    assert_eq!(report.missing_originals, 1);
    let mut s = Session::new();
    s.open_library(&library, false)?;
    let p = s.catalog.photo(PhotoId(1)).context("relocated photo")?;
    match &p.source {
        lightcraft_engine::catalog::Source::File { path } => {
            assert_eq!(Path::new(path), new_root.join("one.ORF"))
        }
        _ => anyhow::bail!("expected file"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_output_cannot_write_into_originals() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    std::os::unix::fs::symlink(temp.path().join("photos"), temp.path().join("alias"))?;
    assert!(
        bundle::extract(
            &src,
            &temp.path().join("alias/forbidden-bundle"),
            None,
            true
        )
        .is_err()
    );
    let path = temp.path().join("bundle");
    bundle::extract(&src, &path, None, true)?;
    assert!(
        migrate::import(
            &path,
            &temp.path().join("alias/forbidden-library"),
            Options::default(),
            None
        )
        .is_err()
    );
    assert!(!temp.path().join("photos/forbidden-bundle").exists());
    assert!(!temp.path().join("photos/forbidden-library").exists());
    Ok(())
}

#[test]
fn pending_commit_does_not_resurrect_a_photo_removed_after_crash() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&src, &path, None, true)?;
    let library = temp.path().join("library");
    migrate::import(&path, &library, Options::default(), None)?;
    let state = library.join("Interop/digikam2lightcraft/state");
    std::fs::rename(
        state.join("batch-000000000001.json.gz"),
        state.join("pending.json.gz"),
    )?;
    let mut s = Session::new();
    s.open_library(&library, false)?;
    s.commit(
        "User removes a photo after the crashed import",
        lightcraft_engine::catalog::Op::RemovePhoto { id: PhotoId(7) },
    )?;
    s.close_library()?;
    drop(s);
    let result = migrate::import(&path, &library, Options::default(), None)?;
    assert_eq!(
        result.added, 0,
        "the native commit marker proves the original import landed"
    );
    assert_eq!(result.removed_from_target, 1);
    assert_eq!(migrate::verify(&path, &library)?["catalogPhotos"], 6);
    Ok(())
}

#[test]
fn pending_without_native_commit_marker_can_be_retried() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let path = temp.path().join("bundle");
    bundle::extract(&src, &path, None, true)?;
    let library = temp.path().join("library");
    migrate::import(&path, &library, Options::default(), None)?;
    let state = library.join("Interop/digikam2lightcraft/state");
    let checkpoint = state.join("batch-000000000001.json.gz");
    let v: serde_json::Value = io::read_gzip(&checkpoint, 128 * 1024 * 1024)?;
    let marker = v["commit_marker"]
        .as_str()
        .context("commit marker")?
        .to_string();
    let mut s = Session::new();
    s.open_library(&library, false)?;
    let mut ops = s
        .catalog
        .stacks()
        .map(|stack| lightcraft_engine::catalog::Op::RemoveStack { id: stack.id })
        .collect::<Vec<_>>();
    ops.extend(
        s.catalog
            .photos()
            .map(|p| lightcraft_engine::catalog::Op::RemovePhoto { id: p.id }),
    );
    ops.push(lightcraft_engine::catalog::Op::SetFolderRecord {
        folder: marker,
        record: None,
    });
    s.commit(
        "Synthetic rollback to the pre-commit state",
        lightcraft_engine::catalog::Op::Batch { ops },
    )?;
    s.close_library()?;
    drop(s);
    std::fs::rename(checkpoint, state.join("pending.json.gz"))?;
    let result = migrate::import(&path, &library, Options::default(), None)?;
    assert_eq!(result.added, 7);
    assert_eq!(migrate::verify(&path, &library)?["catalogPhotos"], 7);
    Ok(())
}

#[test]
fn renamed_required_columns_fail_instead_of_silently_importing_an_empty_library() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    sql(
        temp.path(),
        "ALTER TABLE Images RENAME COLUMN status TO old_status;",
    )?;
    let error = match source(temp.path()) {
        Ok(_) => anyhow::bail!("renamed required status must be rejected"),
        Err(e) => e,
    };
    assert!(error.to_string().contains("required column status"));
    Ok(())
}

#[test]
fn wal_appearing_after_inspection_rejects_extraction() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    std::fs::write(
        temp.path().join("database/digikam4.db-wal"),
        b"new transaction",
    )?;
    let output = temp.path().join("bundle");
    assert!(bundle::extract(&src, &output, None, true).is_err());
    assert!(!output.exists());
    Ok(())
}

#[test]
fn reports_cannot_overwrite_data_or_write_inside_photo_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let original = io::hash_file(&temp.path().join("database/digikam4.db"))?;
    let out = temp.path().join("photos/forbidden.json");
    let response = Command::new(env!("CARGO_BIN_EXE_digikam2lightcraft"))
        .args(["inspect", "--database"])
        .arg(temp.path().join("database/digikam4.db"))
        .arg("--root")
        .arg(format!("1={}", temp.path().join("photos").display()))
        .arg("--output")
        .arg(&out)
        .output()?;
    assert!(!response.status.success());
    assert!(!out.exists());
    let existing = temp.path().join("keep.json");
    std::fs::write(&existing, "keep")?;
    assert!(io::new_report(&existing, &serde_json::json!({}), &[]).is_err());
    assert_eq!(std::fs::read_to_string(existing)?, "keep");
    assert_eq!(
        io::hash_file(&temp.path().join("database/digikam4.db"))?,
        original
    );
    Ok(())
}

#[test]
fn exposure_metadata_preserves_decimal_seconds_and_rejects_float_underflow() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fixture(temp.path())?;
    let src = source(temp.path())?;
    let mut r = src.record(0, false)?;
    r.data
        .get_mut("ImageMetadata")
        .and_then(|v| v.first_mut())
        .context("camera metadata")?
        .insert("exposureTime".to_string(), serde_json::json!(0.4));
    let (photo, _) = adapter::map(&r, PhotoId(1), "2026-10-20T12:00:00", &Options::default())?;
    assert_eq!(photo.meta.shutter, "0.4", "0.4 seconds is not 1/2 second");
    let md = r
        .data
        .get_mut("ImageMetadata")
        .and_then(|v| v.first_mut())
        .context("camera metadata")?;
    md.insert("exposureTime".to_string(), serde_json::json!(1e-320));
    md.insert("aperture".to_string(), serde_json::json!(1e-50));
    let (photo, _) = adapter::map(&r, PhotoId(1), "2026-10-20T12:00:00", &Options::default())?;
    assert!(!photo.meta.shutter.contains("inf"));
    assert_eq!(photo.meta.aperture, None);
    Ok(())
}
