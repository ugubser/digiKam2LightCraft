//! Filesystem reconciliation is separate from catalog migration: never resurrect trash or
//! silently import files that the source catalog does not currently manage.
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{bundle, io::each_record, model::Record};

pub fn inspect(path: &Path) -> Result<Value> {
    let (path, manifest) = bundle::open(path)?;
    ensure!(
        !manifest.sample,
        "filesystem inventory needs a full bundle, not a sample"
    );
    let mut known = HashSet::new();
    each_record::<Record>(&path.join("records.jsonl.gz"), |r| {
        known.insert(r.path);
        Ok(())
    })?;
    let mut uncatalogued = Vec::new();
    let mut missing = Vec::new();
    let mut extensions = BTreeMap::<String, usize>::new();
    for p in &known {
        if !p.is_file() {
            missing.push(p.clone());
        }
    }
    let mut dirs = manifest
        .roots
        .iter()
        .map(|r| (r.path.clone(), 0usize))
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut skipped_symlinks = 0;
    let mut skipped_trash = 0;
    let mut media = 0;
    let mut xmp = 0;
    while let Some((dir, depth)) = dirs.pop() {
        ensure!(
            depth <= 64,
            "filesystem tree exceeds 64 levels at {}",
            dir.display()
        );
        if !visited.insert(dir.clone()) {
            continue;
        }
        ensure!(
            visited.len() <= 100_000,
            "filesystem inventory exceeds 100,000 directories"
        );
        for entry in
            fs::read_dir(&dir).with_context(|| format!("read directory {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("._") {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                skipped_symlinks += 1;
                continue;
            }
            if kind.is_dir() {
                if [".dtrash", ".trash", ".trashes"].contains(&name.to_lowercase().as_str())
                    || name.to_lowercase().starts_with(".trash-")
                {
                    skipped_trash += 1;
                    continue;
                }
                dirs.push((entry.path(), depth.saturating_add(1)));
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let p = entry.path();
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext == "xmp" {
                xmp += 1;
                continue;
            }
            let supported = lightcraft_engine::import::is_supported(&p)
                || [
                    "jpe", "hif", "mov", "mp4", "avi", "mpeg", "mpg", "mpe", "mts", "m2ts", "3gp",
                    "3g2", "m4v", "mkv", "webm", "wmv", "vob", "mxf",
                ]
                .contains(&ext.as_str());
            if supported {
                media += 1;
                if !known.contains(&p) {
                    *extensions.entry(ext).or_default() += 1;
                    uncatalogued.push(p);
                    ensure!(
                        uncatalogued.len() <= 1_000_000,
                        "more than one million uncatalogued files"
                    );
                }
            }
        }
    }
    uncatalogued.sort();
    missing.sort();
    Ok(
        json!({"sourceUuid":manifest.source_uuid,"activeCatalogRecords":known.len(),"mediaFiles":media,"xmpFiles":xmp,"uncataloguedCount":uncatalogued.len(),"uncatalogued":uncatalogued,"uncataloguedExtensions":extensions,"missingCount":missing.len(),"missing":missing,"directories":visited.len(),"skippedTrashDirectories":skipped_trash,"skippedSymlinks":skipped_symlinks}),
    )
}
