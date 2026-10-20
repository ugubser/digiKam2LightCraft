use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use flate2::{Compression, write::GzEncoder};
use lightcraft_engine::{
    Session,
    catalog::{Op, Photo, PhotoId, Stack},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adapter::{self, Notice, Options};
use crate::bundle;
use crate::io::{
    atomic_json, digest, each_record, read_bounded, read_gzip, sync_parent, write_gzip,
};
use crate::model::{ADAPTER, Manifest, Record};

const STATE_LIMIT: u64 = 128 * 1024 * 1024;
const BATCH: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Owner {
    format: String,
    version: u32,
    source_uuid: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub target_id: u64,
    pub baseline: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Update {
    source_id: i64,
    previous: Option<Entry>,
    next: Entry,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Checkpoint {
    source_uuid: String,
    sequence: u64,
    adapter: String,
    updates: Vec<Update>,
    #[serde(default)]
    commit_marker: Option<String>,
}
#[derive(Default)]
struct Index {
    entries: BTreeMap<i64, Entry>,
    sequence: u64,
}
#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub adapter: String,
    pub options: Options,
    pub source_records: usize,
    pub processed: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub preserved_user_fields: usize,
    pub removed_from_target: usize,
    pub missing_originals: usize,
    pub groups_added: usize,
    pub notice_counts: BTreeMap<String, usize>,
    pub examples: Vec<Notice>,
    pub library: PathBuf,
}
struct Reporter {
    report: Report,
    writer: GzEncoder<std::io::BufWriter<File>>,
}
impl Reporter {
    fn notice(&mut self, notice: Notice) -> Result<()> {
        *self
            .report
            .notice_counts
            .entry(notice.code.clone())
            .or_default() += 1;
        serde_json::to_writer(&mut self.writer, &notice)?;
        self.writer.write_all(b"\n")?;
        if self.report.examples.len() < 60
            && self
                .report
                .examples
                .iter()
                .filter(|n| n.code == notice.code)
                .count()
                < 3
        {
            self.report.examples.push(notice);
        }
        Ok(())
    }
    fn finish(self, path: &Path) -> Result<Report> {
        let mut writer = self.writer.finish()?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        atomic_json(path, &self.report)?;
        Ok(self.report)
    }
}

fn hashes(photo: &Photo) -> Result<BTreeMap<String, String>> {
    adapter::groups(photo)?
        .into_iter()
        .map(|(k, v)| Ok((k, digest(&serde_json::to_vec(&v)?))))
        .collect()
}

fn load_index(dir: &Path, uuid: &str) -> Result<Index> {
    let mut paths = fs::read_dir(dir)?
        .map(|r| r.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("batch-"))
                && p.extension().is_some_and(|x| x == "gz")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut index = Index::default();
    for path in paths {
        let checkpoint: Checkpoint = read_gzip(&path, STATE_LIMIT)
            .with_context(|| format!("read migration checkpoint {}", path.display()))?;
        ensure!(
            checkpoint.source_uuid == uuid
                && checkpoint.sequence == index.sequence.saturating_add(1),
            "migration checkpoint sequence/identity mismatch"
        );
        for update in checkpoint.updates {
            index.entries.insert(update.source_id, update.next);
        }
        index.sequence = checkpoint.sequence;
    }
    Ok(index)
}
fn checkpoint_path(dir: &Path, sequence: u64) -> PathBuf {
    dir.join(format!("batch-{sequence:012}.json.gz"))
}
fn stage_checkpoint(dir: &Path, checkpoint: &Checkpoint) -> Result<()> {
    let temp = dir.join(format!("pending.part-{}", std::process::id()));
    write_gzip(&temp, checkpoint)?;
    fs::rename(temp, dir.join("pending.json.gz"))?;
    sync_parent(&dir.join("pending.json.gz"))
}
fn finish_checkpoint(dir: &Path, checkpoint: Checkpoint, index: &mut Index) -> Result<()> {
    let path = checkpoint_path(dir, checkpoint.sequence);
    ensure!(!path.exists(), "checkpoint already exists");
    fs::rename(dir.join("pending.json.gz"), &path)?;
    sync_parent(&path)?;
    for update in checkpoint.updates {
        index.entries.insert(update.source_id, update.next);
    }
    index.sequence = checkpoint.sequence;
    Ok(())
}
fn recover_pending(dir: &Path, index: &mut Index, session: &Session, uuid: &str) -> Result<()> {
    let path = dir.join("pending.json.gz");
    if !path.exists() {
        return Ok(());
    }
    let mut checkpoint: Checkpoint = read_gzip(&path, STATE_LIMIT)?;
    ensure!(
        checkpoint.source_uuid == uuid && checkpoint.sequence == index.sequence.saturating_add(1),
        "pending checkpoint identity/sequence mismatch"
    );
    if let Some(marker) = &checkpoint.commit_marker {
        if session.catalog.folder_record(marker).is_none() {
            checkpoint.updates = checkpoint
                .updates
                .into_iter()
                .filter_map(|mut update| {
                    update.previous.clone().map(|old| {
                        update.next = old;
                        update
                    })
                })
                .collect();
            stage_checkpoint(dir, &checkpoint)?;
        }
        return finish_checkpoint(dir, checkpoint, index);
    }
    // Compatibility with pre-marker checkpoints. Published historical batches are unaffected.
    let mut recovered = Vec::new();
    for mut update in checkpoint.updates {
        let Some(photo) = session.catalog.photo(PhotoId(update.next.target_id)) else {
            continue;
        };
        let current = hashes(photo)?;
        if update.previous.is_none() && current.get("source") != update.next.baseline.get("source")
        {
            continue;
        }
        // If the catalog commit never landed, retain the old baseline; if it did land but a
        // person edited afterwards, their differing values still won't match the new baseline.
        if let Some(previous) = &update.previous {
            for (group, new) in &mut update.next.baseline {
                if current.get(group) != Some(new)
                    && let Some(old) = previous.baseline.get(group)
                {
                    *new = old.clone();
                }
            }
        }
        recovered.push(update);
    }
    checkpoint.updates = recovered;
    // Rewrite the reconciled pending file atomically before publishing it as a checkpoint.
    stage_checkpoint(dir, &checkpoint)?;
    finish_checkpoint(dir, checkpoint, index)
}

fn validate_destination(path: &Path, bundle: &Path, manifest: &Manifest) -> Result<PathBuf> {
    let physical = crate::io::physical_absolute(path)?;
    ensure!(
        !physical.starts_with(bundle),
        "library must be outside the migration bundle"
    );
    for root in &manifest.roots {
        ensure!(
            !physical.starts_with(&root.path),
            "library must be outside original photo roots"
        );
    }
    ensure!(
        !physical.starts_with(
            manifest
                .source_db
                .parent()
                .context("invalid source database path")?
        ),
        "library must be outside the source database directory"
    );
    Ok(physical)
}

pub fn import(
    bundle_path: &Path,
    library: &Path,
    mut options: Options,
    limit: Option<usize>,
) -> Result<Report> {
    let (bundle_path, manifest) = bundle::open(bundle_path)?;
    let mut root_ids = std::collections::HashSet::new();
    for root in &mut options.roots {
        ensure!(
            root_ids.insert(root.id),
            "duplicate import root override {}",
            root.id
        );
        ensure!(
            manifest.roots.iter().any(|r| r.id == root.id),
            "unknown import root override {}",
            root.id
        );
        root.path = root
            .path
            .canonicalize()
            .context("override photo root must exist")?;
        ensure!(root.path.is_dir(), "override photo root is not a directory");
    }
    let mut protected_manifest = manifest.clone();
    protected_manifest.roots.extend(options.roots.clone());
    let library = validate_destination(library, &bundle_path, &protected_manifest)?;
    let interop = library.join("Interop/digikam2lightcraft");
    let owner_path = interop.join("owner.json");
    let existing = owner_path.exists();
    if existing {
        let owner: Owner = serde_json::from_slice(&read_bounded(&owner_path, 1024 * 1024)?)?;
        ensure!(
            owner.format == "digikam2lightcraft-library"
                && owner.version == 1
                && owner.source_uuid == manifest.source_uuid,
            "destination belongs to another migration/source or state version"
        );
    } else {
        ensure!(
            !library.exists() || fs::read_dir(&library)?.next().is_none(),
            "refusing to adopt a nonempty unmanaged library; choose a new directory"
        );
        fs::create_dir_all(interop.parent().context("missing Interop parent")?)?;
        fs::create_dir(&interop)
            .context("another migration may be initializing this destination")?;
        atomic_json(
            &owner_path,
            &Owner {
                format: "digikam2lightcraft-library".to_string(),
                version: 1,
                source_uuid: manifest.source_uuid.clone(),
            },
        )?;
    }
    let state_dir = interop.join("state");
    fs::create_dir_all(&state_dir)?;
    let mut session = Session::new();
    session.open_library(&library, false)?;
    let original_auto_write = session.xmp.auto_write;
    session.xmp.auto_write = false;
    if !existing {
        session.save_prefs()?;
    }
    for warning in session.take_library_warnings() {
        ensure!(
            !warning.contains("could not be locked"),
            "destination locking is unavailable: {warning}"
        );
        eprintln!("LightCraft: {warning}");
    }
    let mut index = load_index(&state_dir, &manifest.source_uuid)?;
    recover_pending(&state_dir, &mut index, &session, &manifest.source_uuid)?;
    let run_dir = interop.join("reports");
    fs::create_dir_all(&run_dir)?;
    let run_id = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
        std::process::id()
    );
    let detail_path = run_dir.join(format!("{run_id}-notices.jsonl.gz"));
    let mut reporter = Reporter {
        report: Report {
            adapter: ADAPTER.to_string(),
            options: options.clone(),
            source_records: manifest.records,
            library: library.clone(),
            ..Default::default()
        },
        writer: GzEncoder::new(
            std::io::BufWriter::new(File::create(detail_path)?),
            Compression::fast(),
        ),
    };
    let mut added_source_ids = std::collections::HashSet::new();
    let groups_marker = state_dir.join("groups-initialized.json");
    let initial_groups_complete = groups_marker.exists();
    let mut ops = Vec::new();
    let mut updates = Vec::new();
    let mut group_members = BTreeMap::<i64, Vec<i64>>::new();
    let now = lightcraft_engine::catalog::rules::now();
    each_record::<Record>(&bundle_path.join("records.jsonl.gz"), |record| {
        if limit.is_some_and(|limit| reporter.report.processed >= limit) {
            return Ok(());
        }
        let mut record = record;
        if let Some(id) = crate::model::integer(record.row("Albums"), "albumRoot")
            && let Some(root) = options.roots.iter().find(|r| r.id == id)
        {
            record.path = crate::io::guarded_join(
                &root.path,
                &crate::model::text(record.row("Albums"), "relativePath"),
                &crate::model::text(record.row("Images"), "name"),
            )?;
        }
        record.present = record.path.is_file();
        reporter.report.processed += 1;
        reporter.report.missing_originals += usize::from(!record.path.is_file());
        if let Some(leader) = record.group_leader {
            group_members
                .entry(leader)
                .or_default()
                .push(record.source_id);
        }
        let previous = index.entries.get(&record.source_id).cloned();
        // Entries in a staged batch are unique, because bundle validation rejects repeated ids.
        let id = previous
            .as_ref()
            .map(|e| PhotoId(e.target_id))
            .unwrap_or_else(|| session.catalog.alloc_photo_id());
        let (desired, notices) = adapter::map(&record, id, &now, &options)?;
        for notice in notices {
            reporter.notice(notice)?;
        }
        if let Some(previous) = &previous {
            let Some(current) = session.catalog.photo(id) else {
                reporter.report.removed_from_target += 1;
                reporter.notice(Notice {
                    source_id: record.source_id,
                    code: "removed-from-lightcraft".to_string(),
                    detail: "not resurrected; existing migration identity retained".to_string(),
                })?;
                return Ok(());
            };
            let (merged, next, protected) = merge(current, &desired, previous)?;
            reporter.report.preserved_user_fields += protected.len();
            for field in protected {
                reporter.notice(Notice {
                    source_id: record.source_id,
                    code: "preserved-lightcraft-edit".to_string(),
                    detail: field,
                })?;
            }
            if &merged != current.as_ref() {
                ops.push(Op::RemovePhoto { id });
                ops.push(Op::AddPhoto {
                    photo: Box::new(merged),
                });
                reporter.report.updated += 1;
            } else {
                reporter.report.unchanged += 1;
            }
            if next.baseline != previous.baseline {
                updates.push(Update {
                    source_id: record.source_id,
                    previous: Some(previous.clone()),
                    next,
                });
            }
        } else {
            let next = Entry {
                target_id: id.0,
                baseline: hashes(&desired)?,
            };
            ops.push(Op::AddPhoto {
                photo: Box::new(desired),
            });
            updates.push(Update {
                source_id: record.source_id,
                previous: None,
                next,
            });
            reporter.report.added += 1;
            added_source_ids.insert(record.source_id);
        }
        if updates.len() >= BATCH {
            flush(
                &mut session,
                &state_dir,
                &manifest.source_uuid,
                &mut index,
                &mut ops,
                &mut updates,
            )?;
        }
        if reporter.report.processed.is_multiple_of(1000) {
            eprintln!(
                "import: {}/{} records ({} added, {} updated, {} protected fields)",
                reporter.report.processed,
                manifest.records,
                reporter.report.added,
                reporter.report.updated,
                reporter.report.preserved_user_fields
            );
        }
        Ok(())
    })?;
    flush(
        &mut session,
        &state_dir,
        &manifest.source_uuid,
        &mut index,
        &mut ops,
        &mut updates,
    )?;
    // Group import is additive: do not change a stack a person may have rearranged. The
    // catalog's stack index is checked once, avoiding a full-library scan for each group.
    let mut already_stacked = session
        .catalog
        .stacks()
        .flat_map(|s| s.photos.iter().copied())
        .collect::<std::collections::HashSet<_>>();
    let mut stack_ops = Vec::new();
    for (leader, mut members) in group_members {
        if initial_groups_complete
            && !added_source_ids.contains(&leader)
            && !members.iter().any(|id| added_source_ids.contains(id))
        {
            continue;
        }

        if index
            .entries
            .get(&leader)
            .is_none_or(|entry| session.catalog.photo(PhotoId(entry.target_id)).is_none())
        {
            reporter.notice(Notice {
                source_id: leader,
                code: "group-leader-unavailable".to_string(),
                detail: "group retained in bundle; original leader is not imported".to_string(),
            })?;
            continue;
        }
        members.sort_unstable();
        members.dedup();
        let mut ids = Vec::new();
        for source_id in
            std::iter::once(leader).chain(members.into_iter().filter(|id| *id != leader))
        {
            if let Some(entry) = index.entries.get(&source_id)
                && session.catalog.photo(PhotoId(entry.target_id)).is_some()
            {
                ids.push(PhotoId(entry.target_id));
            }
        }
        if ids.len() < 2 || ids.iter().any(|id| already_stacked.contains(id)) {
            continue;
        }
        for photo in &ids {
            already_stacked.insert(*photo);
        }
        let id = session.catalog.alloc_stack_id();
        stack_ops.push(Op::AddStack {
            stack: Stack {
                id,
                photos: ids,
                collapsed: true,
            },
        });
        reporter.report.groups_added += 1;
    }
    if !stack_ops.is_empty() {
        session.execute_fn("digikam.importStacks", |s| {
            s.commit("Import digiKam groups", Op::Batch { ops: stack_ops })?;
            Ok(json!({}))
        })?;
    }
    session.xmp.auto_write = original_auto_write;
    session.close_library()?;
    if !initial_groups_complete {
        atomic_json(
            &groups_marker,
            &json!({"sourceUuid":manifest.source_uuid,"adapter":ADAPTER}),
        )?;
    }
    reporter.finish(&run_dir.join(format!("{run_id}-summary.json")))
}

pub fn merge(
    current: &Photo,
    desired: &Photo,
    previous: &Entry,
) -> Result<(Photo, Entry, Vec<String>)> {
    let current_values = adapter::groups(current)?;
    let desired_values = adapter::groups(desired)?;
    let current_hash = hashes(current)?;
    let desired_hash = hashes(desired)?;
    let mut apply = BTreeMap::new();
    let mut next = previous.clone();
    let mut protected = Vec::new();
    for (field, wanted) in &desired_values {
        let current_hash = current_hash
            .get(field)
            .context("missing current fingerprint")?;
        let desired_hash = desired_hash
            .get(field)
            .context("missing desired fingerprint")?;
        if current_values.get(field) == Some(wanted) {
            next.baseline.insert(field.clone(), desired_hash.clone());
        } else if previous.baseline.get(field) == Some(current_hash) {
            apply.insert(field.clone(), wanted.clone());
            next.baseline.insert(field.clone(), desired_hash.clone());
        } else {
            protected.push(field.clone());
        }
    }
    let mut merged = adapter::replace_groups(current, &apply)?;
    if apply.contains_key("develop") {
        merged.edited = desired.edited.clone();
    }
    Ok((merged, next, protected))
}
fn flush(
    session: &mut Session,
    dir: &Path,
    uuid: &str,
    index: &mut Index,
    ops: &mut Vec<Op>,
    updates: &mut Vec<Update>,
) -> Result<()> {
    if updates.is_empty() {
        ensure!(
            ops.is_empty(),
            "catalog operations lack migration checkpoint"
        );
        return Ok(());
    }
    let checkpoint = Checkpoint {
        source_uuid: uuid.to_string(),
        sequence: index
            .sequence
            .checked_add(1)
            .context("checkpoint sequence exhausted")?,
        adapter: ADAPTER.to_string(),
        updates: std::mem::take(updates),
        commit_marker: Some(
            dir.join("commits")
                .join(format!("{:012}", index.sequence.saturating_add(1)))
                .to_string_lossy()
                .to_string(),
        ),
    };
    stage_checkpoint(dir, &checkpoint)?;
    if let Some(marker) = &checkpoint.commit_marker {
        ops.push(Op::SetFolderRecord {
            folder: marker.clone(),
            record: Some(lightcraft_engine::catalog::FolderRecord {
                label: Some(lightcraft_engine::catalog::ColorLabel::Green),
            }),
        });
    }
    if !ops.is_empty() {
        let ops = std::mem::take(ops);
        session.execute_fn("digikam.import", |s| {
            s.commit("Import digiKam metadata", Op::Batch { ops })?;
            Ok(json!({}))
        })?;
    }
    finish_checkpoint(dir, checkpoint, index)
}

pub fn verify(bundle_path: &Path, library: &Path) -> Result<Value> {
    let (bundle_path, manifest) = bundle::open(bundle_path)?;
    let interop = library.join("Interop/digikam2lightcraft");
    let owner: Owner =
        serde_json::from_slice(&read_bounded(&interop.join("owner.json"), 1024 * 1024)?)?;
    ensure!(
        owner.source_uuid == manifest.source_uuid,
        "source/destination identity mismatch"
    );
    let mut session = Session::new();
    session.open_library(library, false)?;
    let index = load_index(&interop.join("state"), &manifest.source_uuid)?;
    ensure!(
        !interop.join("state/pending.json.gz").exists(),
        "unfinished import: rerun import to recover its pending checkpoint"
    );
    let mut imported = 0;
    let mut not_imported = 0;
    let mut removed = 0;
    let mut changed_fields = 0;
    let mut missing = 0;
    each_record::<Record>(&bundle_path.join("records.jsonl.gz"), |r| {
        if let Some(e) = index.entries.get(&r.source_id) {
            if let Some(p) = session.catalog.photo(PhotoId(e.target_id)) {
                imported += 1;
                let current = hashes(p)?;
                changed_fields += e
                    .baseline
                    .iter()
                    .filter(|(k, v)| current.get(*k) != Some(*v))
                    .count();
                if let lightcraft_engine::catalog::Source::File { path } = &p.source {
                    missing += usize::from(!Path::new(path).is_file());
                }
            } else {
                removed += 1;
            }
        } else {
            not_imported += 1;
        }
        Ok(())
    })?;
    // Verification opens with the native lock and does not save/rewrite catalog data.
    Ok(
        json!({"sourceRecords":manifest.records,"imported":imported,"notImported":not_imported,"removedFromLightcraft":removed,"changedLightcraftFields":changed_fields,"missingOriginals":missing,"catalogPhotos":session.catalog.len(),"checkpoints":index.sequence}),
    )
}
