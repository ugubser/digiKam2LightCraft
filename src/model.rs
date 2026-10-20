use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Row = BTreeMap<String, Value>;
pub const BUNDLE_VERSION: u32 = 1;
pub const ADAPTER: &str = "lightcraft-0.9-v2";
pub const MAX_LINE: u64 = 64 * 1024 * 1024;
pub const MAX_PACKET: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RootMapping {
    pub id: i64,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub source_uuid: String,
    pub source_db: PathBuf,
    pub source_sha256: String,
    pub records_sha256: String,
    pub source_schema: String,
    pub roots: Vec<RootMapping>,
    pub created: String,
    pub records: usize,
    pub active_records: usize,
    pub sample: bool,
    pub includes_sidecars: bool,
    pub table_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Packet {
    #[serde(default = "yes")]
    pub applicable: bool,
    pub path: PathBuf,
    pub sha256: String,
    /// UTF-8 XMP verbatim, or base64 for packets with another encoding.
    pub encoding: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub source_id: i64,
    pub path: PathBuf,
    pub present: bool,
    pub data: BTreeMap<String, Vec<Row>>,
    pub tags: Vec<Tag>,
    pub sidecars: Vec<Packet>,
    pub group_leader: Option<i64>,
}

impl Record {
    pub fn row(&self, table: &str) -> Option<&Row> {
        self.data.get(table)?.first()
    }
    pub fn rows(&self, table: &str) -> impl Iterator<Item = &Row> {
        self.data.get(table).into_iter().flatten()
    }
}

pub fn text(row: Option<&Row>, key: &str) -> String {
    row.and_then(|r| r.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
pub fn integer(row: Option<&Row>, key: &str) -> Option<i64> {
    row?.get(key)?.as_i64()
}
pub fn number(row: Option<&Row>, key: &str) -> Option<f64> {
    row?.get(key)?.as_f64().filter(|x| x.is_finite())
}

fn yes() -> bool {
    true
}
