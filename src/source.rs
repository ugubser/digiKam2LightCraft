//! Read-only digiKam schema adapter. No digiKam implementation code is used.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::io::{digest, guarded_join, hash_file, read_bounded};
use crate::model::{MAX_PACKET, Packet, Record, RootMapping, Row, Tag, integer, text};
use crate::sqlite::{Database, Value as SqlValue};

const DATABASE_LIMIT: u64 = 1024 * 1024 * 1024;
const PHOTO_TABLES: &[&str] = &[
    "ImageInformation",
    "ImageMetadata",
    "ImagePositions",
    "ImageComments",
    "ImageCopyright",
    "ImageProperties",
    "ImageHistory",
    "ImageTagProperties",
    "VideoMetadata",
];

pub struct Source {
    pub path: PathBuf,
    pub sha256: String,
    pub uuid: String,
    pub schema: String,
    pub table_counts: BTreeMap<String, usize>,
    pub roots: Vec<RootMapping>,
    pub images: Vec<Row>,
    albums: HashMap<i64, Row>,
    indexed: HashMap<String, HashMap<i64, Vec<Row>>>,
    tags: HashMap<i64, Tag>,
    image_tags: HashMap<i64, Vec<i64>>,
    groups: HashMap<i64, i64>,
    stem_owners: HashMap<(i64, String), i64>,
}

impl Source {
    pub fn ensure_unchanged(&self) -> Result<()> {
        ensure_no_wal(&self.path)?;
        ensure!(
            hash_file(&self.path)? == self.sha256,
            "database changed during extraction; retry with a stable backup"
        );
        Ok(())
    }

    pub fn open(path: &Path, roots: Vec<RootMapping>) -> Result<Self> {
        let path = path.canonicalize().context("source database must exist")?;
        ensure!(
            !roots.is_empty(),
            "provide at least one --root ID=/absolute/photo/root mapping"
        );
        let mut root_ids = HashSet::new();
        let mut roots = roots;
        for root in &mut roots {
            ensure!(
                root_ids.insert(root.id),
                "duplicate root mapping {}",
                root.id
            );
            root.path = root
                .path
                .canonicalize()
                .context("mapped photo root must exist")?;
            ensure!(root.path.is_dir(), "root is not a directory");
        }
        ensure_no_wal(&path)?;
        let main = read_bounded(&path, DATABASE_LIMIT)?;
        let sha256 = digest(&main);
        let db = Database::open_with_wal(main, &[]).map_err(anyhow::Error::msg)?;
        let mut tables = HashMap::<String, Vec<Row>>::new();
        let mut table_counts = BTreeMap::new();
        for table in db.live_tables().map_err(anyhow::Error::msg)? {
            let columns = table
                .column_names
                .context("table has unsupported column declarations")?;
            let required: &[&str] = match table.name.as_str() {
                "Settings" => &["keyword", "value"],
                "AlbumRoots" => &["id"],
                "Albums" => &["id", "albumRoot", "relativePath"],
                "Images" => &["id", "album", "name", "status", "category"],
                "ImageInformation" => &[
                    "imageid",
                    "rating",
                    "width",
                    "height",
                    "orientation",
                    "format",
                    "creationDate",
                ],
                "ImageTags" => &["imageid", "tagid"],
                "Tags" => &["id", "pid", "name"],
                "TagProperties" => &["tagid", "property", "value"],
                "ImageRelations" => &["subject", "object", "type"],
                name if PHOTO_TABLES.contains(&name) => &["imageid"],
                _ => &[],
            };
            for column in required {
                ensure!(
                    columns.iter().any(|name| name == column),
                    "unsupported digiKam schema: {} is missing required column {column}",
                    table.name
                );
            }
            let records = db
                .read_table(table.rootpage, columns.len())
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("table {}", table.name))?;
            table_counts.insert(table.name.clone(), records.len());
            // Preserve all tables byte-for-byte in the archived SQLite file. Only decode the
            // current adapter's tables into memory; unknown tables are deliberately not mapped.
            if !PHOTO_TABLES.contains(&table.name.as_str())
                && ![
                    "Images",
                    "Albums",
                    "AlbumRoots",
                    "Settings",
                    "Tags",
                    "TagProperties",
                    "ImageTags",
                    "ImageRelations",
                ]
                .contains(&table.name.as_str())
            {
                continue;
            }
            let rows = records
                .into_iter()
                .map(|record| {
                    columns
                        .iter()
                        .cloned()
                        .zip(record.values.into_iter().map(|v| match v {
                            SqlValue::Null => Value::Null,
                            SqlValue::Integer(n) => json!(n),
                            SqlValue::Real(n) => json!(n),
                            SqlValue::Text(s) => json!(s),
                            SqlValue::Blob(b) => json!({"base64": STANDARD.encode(b)}),
                        }))
                        .collect()
                })
                .collect();
            tables.insert(table.name, rows);
        }
        for required in [
            "Images",
            "Albums",
            "AlbumRoots",
            "ImageInformation",
            "Settings",
        ] {
            ensure!(
                tables.contains_key(required),
                "not a supported digiKam database: missing {required}"
            );
        }
        let settings = tables.remove("Settings").unwrap_or_default();
        let setting = |key: &str| {
            settings
                .iter()
                .find(|r| text(Some(r), "keyword") == key)
                .map(|r| text(Some(r), "value"))
                .unwrap_or_default()
        };
        let uuid = setting("databaseUUID");
        ensure!(
            !uuid.is_empty(),
            "source has no databaseUUID; cannot assign stable migration identities"
        );
        let schema = setting("DBVersion");
        let images = tables.remove("Images").unwrap_or_default();
        let albums = keyed(tables.remove("Albums").unwrap_or_default(), "id")?;
        let album_roots = tables.remove("AlbumRoots").unwrap_or_default();
        for root in &roots {
            ensure!(
                album_roots
                    .iter()
                    .any(|r| integer(Some(r), "id") == Some(root.id)),
                "unknown source root {}",
                root.id
            );
        }
        let tags_raw = keyed(tables.remove("Tags").unwrap_or_default(), "id")?;
        let mut properties: HashMap<i64, BTreeMap<String, String>> = HashMap::new();
        for row in tables.remove("TagProperties").unwrap_or_default() {
            let id = integer(Some(&row), "tagid").context("tag property has no id")?;
            properties
                .entry(id)
                .or_default()
                .insert(text(Some(&row), "property"), text(Some(&row), "value"));
        }
        let mut tags = HashMap::new();
        for (id, row) in &tags_raw {
            let mut parts = Vec::new();
            let mut current = *id;
            let mut seen = HashSet::new();
            while current != 0 {
                ensure!(
                    seen.len() < 128 && seen.insert(current),
                    "tag hierarchy contains a cycle or exceeds 128 levels at {id}"
                );
                let t = tags_raw.get(&current).context("tag has a missing parent")?;
                let name = text(Some(t), "name");
                // The original component remains in the raw database; report separators rather
                // than silently inventing a different target hierarchy.
                ensure!(
                    !name.contains('|'),
                    "tag {current} contains LightCraft's hierarchy separator '|'"
                );
                parts.push(name);
                current = integer(Some(t), "pid").unwrap_or(0);
            }
            parts.reverse();
            tags.insert(
                *id,
                Tag {
                    id: *id,
                    path: parts.join("|"),
                    name: text(Some(row), "name"),
                    properties: properties.remove(id).unwrap_or_default(),
                },
            );
        }
        let mut image_tags: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in tables.remove("ImageTags").unwrap_or_default() {
            if let (Some(image), Some(tag)) =
                (integer(Some(&row), "imageid"), integer(Some(&row), "tagid"))
            {
                image_tags.entry(image).or_default().push(tag);
            }
        }
        let mut groups = HashMap::new();
        for row in tables.remove("ImageRelations").unwrap_or_default() {
            if integer(Some(&row), "type") == Some(2)
                && let (Some(member), Some(leader)) = (
                    integer(Some(&row), "subject"),
                    integer(Some(&row), "object"),
                )
            {
                ensure!(
                    groups.insert(member, leader).is_none(),
                    "image {member} has multiple group leaders"
                );
            }
        }
        let mut indexed = HashMap::new();
        for name in PHOTO_TABLES {
            let mut by_id: HashMap<i64, Vec<Row>> = HashMap::new();
            for row in tables.remove(*name).unwrap_or_default() {
                if let Some(id) = integer(Some(&row), "imageid") {
                    by_id.entry(id).or_default().push(row);
                }
            }
            indexed.insert(name.to_string(), by_id);
        }
        let mut owners = HashMap::<(i64, String), (bool, String, i64)>::new();
        for image in &images {
            if integer(Some(image), "status") != Some(1) {
                continue;
            }
            if let (Some(album), Some(id)) =
                (integer(Some(image), "album"), integer(Some(image), "id"))
            {
                let name = text(Some(image), "name");
                let stem = Path::new(&name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let raw = indexed
                    .get("ImageInformation")
                    .and_then(|m| m.get(&id))
                    .and_then(|r| r.first())
                    .is_some_and(|r| text(Some(r), "format").starts_with("RAW-"));
                let rank = (!raw, name.to_lowercase(), id);
                let entry = owners.entry((album, stem)).or_insert_with(|| rank.clone());
                if rank < *entry {
                    *entry = rank;
                }
            }
        }
        let stem_owners = owners
            .into_iter()
            .map(|(key, (_, _, id))| (key, id))
            .collect();
        ensure_no_wal(&path)?;
        ensure!(
            hash_file(&path)? == sha256,
            "database changed while being read; retry with a stable backup"
        );
        Ok(Self {
            path,
            sha256,
            uuid,
            schema,
            table_counts,
            roots,
            images,
            albums,
            indexed,
            tags,
            image_tags,
            groups,
            stem_owners,
        })
    }

    pub fn active(&self) -> Vec<usize> {
        self.images
            .iter()
            .enumerate()
            .filter_map(|(i, r)| (integer(Some(r), "status") == Some(1)).then_some(i))
            .collect()
    }
    pub fn record(&self, index: usize, sidecars: bool) -> Result<Record> {
        let image = self.images.get(index).context("image index out of range")?;
        let id = integer(Some(image), "id").context("image has no id")?;
        let album_id = integer(Some(image), "album").context("active image has no album")?;
        let album = self
            .albums
            .get(&album_id)
            .context("active image has no album record")?;
        let root_id = integer(Some(album), "albumRoot").context("album has no root id")?;
        let root = self
            .roots
            .iter()
            .find(|root| root.id == root_id)
            .with_context(|| format!("no --root mapping for active album root {root_id}"))?;
        let name = text(Some(image), "name");
        let path = guarded_join(&root.path, &text(Some(album), "relativePath"), &name)?;
        let mut data = BTreeMap::new();
        data.insert("Images".to_string(), vec![image.clone()]);
        data.insert("Albums".to_string(), vec![album.clone()]);
        for table in PHOTO_TABLES {
            if let Some(rows) = self.indexed.get(*table).and_then(|m| m.get(&id)) {
                data.insert(table.to_string(), rows.clone());
            }
        }
        let mut tags = self
            .image_tags
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.tags.get(id))
            .cloned()
            .collect::<Vec<_>>();
        // Region tags need not appear in ImageTags (Unknown faces in this backup do not).
        for row in data.get("ImageTagProperties").into_iter().flatten() {
            if let Some(tag) = integer(Some(row), "tagid").and_then(|id| self.tags.get(&id))
                && !tags.iter().any(|t| t.id == tag.id)
            {
                tags.push(tag.clone());
            }
        }
        tags.sort_by_key(|t| t.id);
        let packets = if sidecars {
            read_packets(
                &path,
                self.stem_owners.get(&(
                    album_id,
                    Path::new(&name)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                )) == Some(&id),
            )?
        } else {
            Vec::new()
        };
        Ok(Record {
            source_id: id,
            present: path.is_file(),
            path,
            data,
            tags,
            sidecars: packets,
            group_leader: self.groups.get(&id).copied(),
        })
    }
}

fn keyed(rows: Vec<Row>, key: &str) -> Result<HashMap<i64, Row>> {
    let mut out = HashMap::new();
    for row in rows {
        let id = integer(Some(&row), key).context("record lacks primary key")?;
        ensure!(out.insert(id, row).is_none(), "duplicate primary key {id}");
    }
    Ok(out)
}

fn read_packets(photo: &Path, owns_stem: bool) -> Result<Vec<Packet>> {
    let mut candidates = Vec::new();
    // Full-name packets take precedence; stem packets remain archived independently.
    for base in [
        PathBuf::from(format!("{}.xmp", photo.display())),
        photo.with_extension("xmp"),
    ] {
        for ext in ["xmp", "XMP", "Xmp"] {
            let candidate = base.with_extension(ext);
            match candidate.try_exists() {
                Ok(true) => {
                    candidates.push((candidate, base != photo.with_extension("xmp") || owns_stem))
                }
                Ok(false) => {}
                Err(e) => bail!("cannot inspect sidecar {}: {e}", candidate.display()),
            }
        }
    }
    let mut out = Vec::new();
    for (path, applicable) in candidates {
        if out.iter().any(|p: &Packet| p.path == path) {
            continue;
        }
        let bytes = read_bounded(&path, MAX_PACKET)?;
        let sha256 = digest(&bytes);
        let (encoding, body) = match String::from_utf8(bytes) {
            Ok(s) => ("utf8", s),
            Err(e) => ("base64", STANDARD.encode(e.into_bytes())),
        };
        out.push(Packet {
            applicable,
            path,
            sha256,
            encoding: encoding.to_string(),
            body,
        });
    }
    Ok(out)
}

fn ensure_no_wal(path: &Path) -> Result<()> {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    match std::fs::metadata(&wal) {
        Ok(m) => ensure!(
            m.len() == 0,
            "source has a WAL: close digiKam and take a consistent SQLite backup first; source was not changed"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot inspect source WAL"),
    }
    Ok(())
}
