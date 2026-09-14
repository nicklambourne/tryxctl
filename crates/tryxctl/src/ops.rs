//! A journal of transfers, and the encodes kept from failed ones so a retry
//! does not pay for ffmpeg twice.

use crate::exit::{self, CommandResult, Failure};
use crate::media::TransformArgs;
use crate::{legacy, media, output};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const KEEP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Running,
    Ok,
    Failed,
}

/// What a transfer needs to be run again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub kind: &'static str,
    pub source: PathBuf,
    /// The `--name` the user passed, if any.
    pub name: Option<String>,
    pub transform: TransformArgs,
    pub show: bool,
    pub replace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub started_unix: i64,
    pub finished_unix: Option<i64>,
    pub source: PathBuf,
    pub name: Option<String>,
    /// The name on the display.
    pub remote: String,
    pub target: String,
    pub transform: TransformArgs,
    pub show: bool,
    pub replace: bool,
    pub outcome: Outcome,
    #[serde(default)]
    pub error: Option<String>,
    /// The encode kept for a retry.
    #[serde(default)]
    pub cached: Option<PathBuf>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
}

pub fn journal_path() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state").map(|base| base.join("tryxctl/operations.json"))
}

pub fn cache_dir() -> Option<PathBuf> {
    xdg("XDG_CACHE_HOME", ".cache").map(|base| base.join("tryxctl/encodes"))
}

pub fn load() -> Vec<Record> {
    journal_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(records: &[Record]) -> std::io::Result<()> {
    let Some(path) = journal_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(records)?)?;
    std::fs::rename(&temp, path)
}

/// A new transfer id: twelve hex digits, distinct even for transfers one
/// process begins within the same second, and evenly spread so that a short
/// prefix names one.
fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{nanos}|{}|{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    Sha256::digest(seed.as_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Journals the start of a transfer.
pub fn begin(pending: &Pending, remote: &str, target: &str) -> Record {
    let started = now_unix();
    let record = Record {
        id: new_id(),
        kind: pending.kind.to_string(),
        started_unix: started,
        finished_unix: None,
        source: pending.source.clone(),
        name: pending.name.clone(),
        remote: remote.to_string(),
        target: target.to_string(),
        transform: pending.transform.clone(),
        show: pending.show,
        replace: pending.replace,
        outcome: Outcome::Running,
        error: None,
        cached: None,
        size: None,
        sha256: None,
    };
    let mut records = load();
    records.push(record.clone());
    if records.len() > KEEP {
        let drop = records.len() - KEEP;
        for old in records.drain(..drop) {
            if let Some(cached) = old.cached {
                let _ = std::fs::remove_file(cached);
            }
        }
    }
    let _ = save(&records);
    record
}

pub fn finish(
    id: &str,
    outcome: Outcome,
    error: Option<String>,
    cached: Option<PathBuf>,
    size: Option<u64>,
    sha256: Option<String>,
) {
    let mut records = load();
    if let Some(record) = records.iter_mut().find(|record| record.id == id) {
        record.finished_unix = Some(now_unix());
        record.outcome = outcome;
        record.error = error;
        record.cached = cached;
        record.size = size;
        record.sha256 = sha256;
    }
    let _ = save(&records);
}

/// A key for the encode of `source` with these options: the same source
/// (path, size, modification time) prepared the same way for the same
/// display name.
pub fn cache_key(
    source: &Path,
    transform: &TransformArgs,
    target: &str,
    remote: &str,
) -> Result<String, Failure> {
    let metadata = std::fs::metadata(source)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    hasher.update(format!("|{}|{modified}|{target}|{remote}|", metadata.len()).as_bytes());
    hasher.update(
        serde_json::to_string(transform)
            .unwrap_or_default()
            .as_bytes(),
    );
    let digest = hasher.finalize();
    Ok(digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The kept encode for `key`, when there is one.
pub fn cached(key: &str) -> Option<PathBuf> {
    let path = cache_dir()?.join(key);
    path.is_file().then_some(path)
}

/// Moves a finished encode into the cache under `key`.
pub fn keep(key: &str, staged: &Path) -> std::io::Result<PathBuf> {
    let dir = cache_dir().ok_or_else(|| std::io::Error::other("no cache directory"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(key);
    if path != staged && std::fs::rename(staged, &path).is_err() {
        std::fs::copy(staged, &path)?;
        let _ = std::fs::remove_file(staged);
    }
    Ok(path)
}

pub fn discard(key: &str) {
    if let Some(path) = cached(key) {
        let _ = std::fs::remove_file(path);
    }
}

fn age(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{s} s ago"),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

/// `op ls`: the journal, newest last.
pub fn ls(json: bool) -> CommandResult {
    let records = load();
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(exit::ok());
    }
    if records.is_empty() {
        println!("No transfers recorded.");
        return Ok(exit::ok());
    }
    let now = now_unix();
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            let detail = match record.outcome {
                Outcome::Ok => record.size.map(output::human_bytes).unwrap_or_default(),
                Outcome::Failed => record.error.clone().unwrap_or_default(),
                Outcome::Running => "in progress".to_string(),
            };
            vec![
                record.id.clone(),
                age(now - record.started_unix),
                record.kind.clone(),
                record.remote.clone(),
                match record.outcome {
                    Outcome::Ok => "ok",
                    Outcome::Failed => "failed",
                    Outcome::Running => "running",
                }
                .to_string(),
                if record.cached.as_ref().is_some_and(|p| p.is_file()) {
                    "kept".to_string()
                } else {
                    String::new()
                },
                detail,
            ]
        })
        .collect();
    print!(
        "{}",
        output::table(
            &["ID", "WHEN", "KIND", "NAME", "OUTCOME", "ENCODE", "DETAIL"],
            &rows
        )
    );
    Ok(exit::ok())
}

/// The record `id` names: its whole id, or a prefix of no other.
fn find(records: Vec<Record>, id: &str) -> Result<Record, Failure> {
    if id.is_empty() {
        return Err(Failure::usage("no transfer id given; see `op ls`"));
    }
    if let Some(record) = records.iter().find(|record| record.id == id) {
        return Ok(record.clone());
    }
    let mut matches: Vec<Record> = records
        .into_iter()
        .filter(|record| record.id.starts_with(id))
        .collect();
    match matches.len() {
        0 => Err(Failure::usage(format!("no transfer {id}; see `op ls`"))),
        1 => Ok(matches.remove(0)),
        count => Err(Failure::usage(format!(
            "{id} begins {count} transfer ids; give more of the one to retry"
        ))),
    }
}

/// `op retry ID`: runs a failed transfer again with the same options; the
/// kept encode is picked up through the cache key.
pub fn retry(json: bool, session: &legacy::Session, id: &str) -> CommandResult {
    let record = find(load(), id)?;
    if record.outcome != Outcome::Failed {
        return Err(Failure::usage(format!(
            "transfer {} is {}; only failed ones can be retried",
            record.id,
            match record.outcome {
                Outcome::Ok => "complete",
                _ => "still running",
            }
        )));
    }
    if !json {
        println!(
            "retrying {} of {} as {}",
            record.kind,
            record.source.display(),
            record.remote
        );
    }
    match record.kind.as_str() {
        "replace" => media::replace(
            json,
            session,
            &record.remote,
            &record.source,
            &record.transform,
        ),
        _ => media::upload(
            json,
            session,
            &record.source,
            record.name.clone(),
            record.show,
            record.replace,
            false,
            false,
            &record.transform,
        ),
    }
}

/// Drops the kept encodes, and the journal too when asked; returns how many
/// encodes were removed.
pub fn remove_kept(journal: bool) -> u64 {
    let mut removed = 0u64;
    if let Some(dir) = cache_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    let mut records = load();
    for record in &mut records {
        record.cached = None;
    }
    if journal {
        records.clear();
    }
    let _ = save(&records);
    removed
}

/// `op clear`: drops the kept encodes, and the journal with `--journal`.
pub fn clear(json: bool, journal: bool) -> CommandResult {
    let removed = remove_kept(journal);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"removed_encodes": removed, "journal_cleared": journal})
            )?
        );
    } else {
        println!(
            "removed {removed} kept encode(s){}",
            if journal { " and the journal" } else { "" }
        );
    }
    Ok(exit::ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> Record {
        Record {
            id: id.to_string(),
            kind: "upload".into(),
            started_unix: 0,
            finished_unix: None,
            source: PathBuf::from("clip.mp4"),
            name: None,
            remote: "clip.mp4".into(),
            target: "legacy-panorama".into(),
            transform: TransformArgs::default(),
            show: false,
            replace: false,
            outcome: Outcome::Failed,
            error: None,
            cached: None,
            size: None,
            sha256: None,
        }
    }

    #[test]
    fn ids_are_distinct_even_within_one_second() {
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 1000);
        for id in ids.iter().take(10) {
            assert_eq!(id.len(), 12);
            assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        }
    }

    #[test]
    fn a_transfer_is_found_by_its_id_or_a_unique_prefix() {
        let records = || {
            vec![
                record("a1b2c3d4e5f6"),
                record("a1b2ffffffff"),
                record("0123"),
            ]
        };
        assert_eq!(find(records(), "a1b2c3d4e5f6").unwrap().id, "a1b2c3d4e5f6");
        assert_eq!(find(records(), "a1b2c").unwrap().id, "a1b2c3d4e5f6");
        assert_eq!(find(records(), "0").unwrap().id, "0123");
        let ambiguous = find(records(), "a1b2").unwrap_err();
        assert!(
            ambiguous.message.contains("begins 2 transfer ids"),
            "{}",
            ambiguous.message
        );
        assert!(find(records(), "").is_err(), "an empty id names nothing");
        assert!(
            find(records(), "ffff")
                .unwrap_err()
                .message
                .contains("no transfer ffff")
        );
        // A whole id wins over being the prefix of a longer one.
        let nested = vec![record("abc"), record("abcdef")];
        assert_eq!(find(nested, "abc").unwrap().id, "abc");
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(0), "0 s ago");
        assert_eq!(age(59), "59 s ago");
        assert_eq!(age(60), "1 min ago");
        assert_eq!(age(7_199), "1 h ago");
        assert_eq!(age(86_400 * 3), "3 d ago");
    }

    #[test]
    fn the_cache_key_changes_with_anything_that_changes_the_encode() {
        let dir = std::env::temp_dir().join(format!("tryxctl-ops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("clip.mp4");
        std::fs::write(&source, b"one").unwrap();
        let plain = TransformArgs::default();
        let key = cache_key(&source, &plain, "legacy-panorama", "clip.mp4").unwrap();
        assert_eq!(key.len(), 24);
        assert_eq!(
            key,
            cache_key(&source, &plain, "legacy-panorama", "clip.mp4").unwrap()
        );
        let rotated = TransformArgs {
            rotate: 90,
            ..TransformArgs::default()
        };
        assert_ne!(
            key,
            cache_key(&source, &rotated, "legacy-panorama", "clip.mp4").unwrap()
        );
        assert_ne!(
            key,
            cache_key(&source, &plain, "kanali-panorama", "clip.mp4").unwrap()
        );
        assert_ne!(
            key,
            cache_key(&source, &plain, "legacy-panorama", "other.mp4").unwrap()
        );
        std::fs::write(&source, b"longer").unwrap();
        assert_ne!(
            key,
            cache_key(&source, &plain, "legacy-panorama", "clip.mp4").unwrap()
        );
        assert!(cache_key(&dir.join("missing.mp4"), &plain, "legacy-panorama", "x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
