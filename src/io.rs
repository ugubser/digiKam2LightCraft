use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::model::MAX_LINE;

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).context("hash buffer length")?);
    }
    Ok(format!("{:x}", h.finalize()))
}
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ensure!(
        f.metadata()?.len() <= limit,
        "{} exceeds {limit} bytes",
        path.display()
    );
    let mut buf = Vec::new();
    f.take(limit.saturating_add(1)).read_to_end(&mut buf)?;
    ensure!(
        buf.len() as u64 <= limit,
        "{} grew beyond {limit} bytes",
        path.display()
    );
    Ok(buf)
}
pub fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("output needs a parent directory")?;
    fs::create_dir_all(parent)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        let mut w = std::io::BufWriter::new(f);
        serde_json::to_writer_pretty(&mut w, value)?;
        w.flush()?;
        w.get_ref().sync_all()?;
        fs::rename(&temp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn write_gzip<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut w = GzEncoder::new(std::io::BufWriter::new(file), Compression::fast());
    serde_json::to_writer(&mut w, value)?;
    let mut w = w.finish()?;
    w.flush()?;
    w.get_ref().sync_all()?;
    sync_parent(path)
}
pub fn read_gzip<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let mut bytes = Vec::new();
    GzDecoder::new(File::open(path)?)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{} decompresses beyond {limit} bytes",
        path.display()
    );
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn each_record<T: DeserializeOwned>(
    path: &Path,
    mut run: impl FnMut(T) -> Result<()>,
) -> Result<usize> {
    let mut reader = BufReader::new(GzDecoder::new(File::open(path)?));
    let mut line = String::new();
    let mut count = 0_usize;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take(MAX_LINE.saturating_add(1))
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        ensure!(
            n as u64 <= MAX_LINE && line.ends_with('\n'),
            "record exceeds size limit or is truncated"
        );
        count = count.checked_add(1).context("record count overflow")?;
        ensure!(count <= 1_000_000, "more than one million records");
        run(serde_json::from_str(&line).with_context(|| format!("invalid record {count}"))?)?;
    }
    Ok(count)
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                ensure!(result.pop(), "path escapes filesystem root");
            }
            std::path::Component::CurDir => {}
            c => result.push(c.as_os_str()),
        }
    }
    Ok(result)
}
pub fn guarded_join(root: &Path, relative: &str, name: &str) -> Result<PathBuf> {
    let rel = relative.trim_start_matches('/');
    ensure!(
        !name.is_empty()
            && !name.contains('/')
            && !name.contains('\\')
            && name != "."
            && name != "..",
        "invalid photo filename {name:?}"
    );
    for c in Path::new(rel).components() {
        if !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        ) {
            bail!("unsafe album path {relative:?}");
        }
    }
    let path = root.join(rel).join(name);
    ensure!(
        physical_absolute(&path)?.starts_with(root.canonicalize()?),
        "photo symlink escapes mapped root: {}",
        path.display()
    );
    Ok(path)
}

pub fn physical_absolute(path: &Path) -> Result<PathBuf> {
    let path = absolute(path)?;
    let mut ancestor = path.as_path();
    let mut tail = Vec::new();
    while !ancestor.try_exists()? {
        tail.push(ancestor.file_name().context("invalid path")?.to_os_string());
        ancestor = ancestor.parent().context("invalid path parent")?;
    }
    let mut physical = ancestor.canonicalize()?;
    for part in tail.into_iter().rev() {
        physical.push(part);
    }
    Ok(physical)
}
