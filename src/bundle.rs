use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use flate2::{Compression, write::GzEncoder};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::io::{atomic_json, each_record, hash_file, read_bounded, sync_parent};
use crate::model::{BUNDLE_VERSION, Manifest, Record, integer, text};
use crate::source::Source;

pub fn inspect(source: &Source) -> Result<Value> {
    let active = source.active();
    let mut formats = BTreeMap::<String, usize>::new();
    let mut missing = Vec::new();
    let mut videos = 0;
    for (n, index) in active.iter().enumerate() {
        let record = source.record(*index, false)?;
        *formats
            .entry(text(record.row("ImageInformation"), "format"))
            .or_default() += 1;
        videos += usize::from(integer(record.row("Images"), "category") == Some(2));
        if !record.present {
            missing.push(json!({"sourceId":record.source_id,"path":record.path}));
        }
        if n > 0 && n % 25_000 == 0 {
            eprintln!("inspect: {n}/{} records", active.len());
        }
    }
    Ok(
        json!({"format":"digikam2lightcraft-inspection","sourceUuid":source.uuid,"schema":source.schema,
        "totalRecords":source.images.len(),"activeRecords":active.len(),"inactiveRecords":source.images.len().saturating_sub(active.len()),
        "photos":active.len().saturating_sub(videos),"videos":videos,"formats":formats,
        "present":active.len().saturating_sub(missing.len()),"missing":missing,"tableCounts":source.table_counts,"roots":source.roots}),
    )
}

pub fn extract(
    source: &Source,
    destination: &Path,
    sample: Option<usize>,
    sidecars: bool,
) -> Result<Manifest> {
    let destination = crate::io::physical_absolute(destination)?;
    ensure!(
        !destination.exists(),
        "bundle already exists: {}; choose a new path",
        destination.display()
    );
    for root in &source.roots {
        ensure!(
            !destination.starts_with(&root.path),
            "bundle must be outside the photo roots"
        );
    }
    ensure!(
        !destination.starts_with(source.path.parent().context("database parent")?),
        "bundle must be outside the source database directory"
    );
    let parent = destination.parent().context("bundle needs a parent")?;
    fs::create_dir_all(parent)?;
    let stage = destination.with_file_name(format!(
        ".{}.partial-{}",
        destination
            .file_name()
            .context("bundle needs a name")?
            .to_string_lossy(),
        std::process::id()
    ));
    fs::create_dir(&stage).context("cannot create exclusive bundle staging directory; remove a previous incomplete staging directory if necessary")?;
    // A failed run deliberately keeps its staging directory for diagnosis. A manifest is only
    // published after all bytes are durable and validated. No partial directory is accepted.
    let active = source.active();
    let selected = select_sample(source, &active, sample)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join("digikam4.db"))?;
    let mut input = File::open(&source.path)?;
    std::io::copy(&mut input, &mut file)?;
    file.sync_all()?;
    ensure!(
        hash_file(&stage.join("digikam4.db"))? == source.sha256,
        "database changed before snapshot; repeat with stable backup"
    );
    let records_path = stage.join("records.jsonl.gz");
    let mut writer = GzEncoder::new(
        BufWriter::with_capacity(1024 * 1024, File::create(&records_path)?),
        Compression::fast(),
    );
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .context("create bounded extraction workers")?;
    let mut done = 0usize;
    for chunk in selected.chunks(16) {
        let records: Result<Vec<Vec<u8>>> = pool.install(|| {
            chunk
                .par_iter()
                .map(|index| {
                    let record = source.record(*index, sidecars)?;
                    let bytes = serde_json::to_vec(&record)?;
                    ensure!(
                        (bytes.len() as u64) < crate::model::MAX_LINE,
                        "source photo {} exceeds the record size limit",
                        record.source_id
                    );
                    Ok(bytes)
                })
                .collect()
        });
        for bytes in records? {
            writer.write_all(&bytes)?;
            writer.write_all(b"\n")?;
        }
        let before = done;
        done = done
            .checked_add(chunk.len())
            .context("extraction count overflow")?;
        if before / 1000 != done / 1000 || done == selected.len() {
            eprintln!("extract: {done}/{} records", selected.len());
        }
    }
    let mut writer = writer.finish()?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    ensure!(
        hash_file(&source.path)? == source.sha256,
        "database changed during extraction; repeat with stable backup"
    );
    let manifest = Manifest {
        format: "digikam2lightcraft-bundle".to_string(),
        version: BUNDLE_VERSION,
        source_uuid: source.uuid.clone(),
        source_db: source.path.clone(),
        source_sha256: source.sha256.clone(),
        records_sha256: hash_file(&records_path)?,
        source_schema: source.schema.clone(),
        roots: source.roots.clone(),
        created: lightcraft_engine::catalog::rules::now(),
        records: selected.len(),
        active_records: active.len(),
        sample: selected.len() != active.len(),
        includes_sidecars: sidecars,
        table_counts: source.table_counts.clone(),
    };
    atomic_json(&stage.join("manifest.json"), &manifest)?;
    fs::rename(&stage, &destination)?;
    sync_parent(&destination)?;
    Ok(manifest)
}

fn select_sample(source: &Source, active: &[usize], sample: Option<usize>) -> Result<Vec<usize>> {
    let Some(n) = sample else {
        return Ok(active.to_vec());
    };
    ensure!(n > 0, "sample must be positive");
    if n >= active.len() {
        return Ok(active.to_vec());
    }
    let mut selected = HashSet::new();
    let mut seen_formats = HashSet::new();
    // Include a representative of every format before spreading samples across the history.
    for index in active {
        let r = source.record(*index, false)?;
        if seen_formats.insert(text(r.row("ImageInformation"), "format")) {
            selected.insert(*index);
        }
        if selected.len() == n {
            break;
        }
    }
    for i in 0..n {
        if selected.len() >= n {
            break;
        }
        let pos = i
            .checked_mul(active.len())
            .context("sample index overflow")?
            / n;
        if let Some(index) = active.get(pos) {
            selected.insert(*index);
        }
    }
    for index in active {
        if selected.len() >= n {
            break;
        }
        selected.insert(*index);
    }
    let mut selected = selected.into_iter().collect::<Vec<_>>();
    selected.sort_unstable();
    Ok(selected)
}

pub fn open(path: &Path) -> Result<(PathBuf, Manifest)> {
    let path = path
        .canonicalize()
        .context("bundle directory does not exist")?;
    let manifest: Manifest =
        serde_json::from_slice(&read_bounded(&path.join("manifest.json"), 4 * 1024 * 1024)?)?;
    ensure!(
        manifest.format == "digikam2lightcraft-bundle" && manifest.version == BUNDLE_VERSION,
        "unsupported migration bundle format/version"
    );
    ensure!(
        !manifest.source_uuid.is_empty() && manifest.records <= 1_000_000,
        "invalid bundle identity or record count"
    );
    ensure!(
        hash_file(&path.join("digikam4.db"))? == manifest.source_sha256,
        "source database snapshot checksum mismatch"
    );
    ensure!(
        hash_file(&path.join("records.jsonl.gz"))? == manifest.records_sha256,
        "record archive checksum mismatch"
    );
    // Parse the entire stream before opening the destination for mutation. A checksummed but
    // malformed or oversized record must not leave an import half-applied.
    let mut ids = HashSet::new();
    let count = each_record::<Record>(&path.join("records.jsonl.gz"), |r| {
        ensure!(
            r.source_id > 0 && ids.insert(r.source_id),
            "duplicate/invalid source photo id {}",
            r.source_id
        );
        ensure!(
            r.path.is_absolute()
                && manifest
                    .roots
                    .iter()
                    .any(|root| r.path.starts_with(&root.path)),
            "record path is outside mapped roots"
        );
        for packet in &r.sidecars {
            let bytes = crate::adapter::packet_bytes(packet)?;
            ensure!(
                crate::io::digest(&bytes) == packet.sha256,
                "sidecar checksum mismatch"
            );
        }
        Ok(())
    })?;
    ensure!(
        count == manifest.records,
        "record count mismatch: expected {}, read {count}",
        manifest.records
    );
    Ok((path, manifest))
}
