//! Opt-in, repository-local product measurements.
//!
//! Measurements live in the repository's Git directory, are never uploaded,
//! and are disabled until the user enables them.

use crate::{queue, RepoInfo};
use fs2::FileExt;
use serde_json::{json, Map, Value};
use std::{fs, io};

const COUNTERS: &[&str] = &[
    "captured_events",
    "extracted_jobs",
    "last_sync_discovered_sessions",
    "last_sync_captured_sessions",
    "review_corrections",
    "handoff_deliveries",
    "repeated_explanations",
];

fn path(repo: &RepoInfo) -> std::path::PathBuf {
    repo.git_dir.join("doclify/metrics.json")
}

fn empty() -> Value {
    let counts = COUNTERS
        .iter()
        .map(|key| ((*key).to_owned(), json!(0)))
        .collect::<Map<_, _>>();
    json!({"version":1,"enabled":false,"counts":counts})
}

fn read(repo: &RepoInfo) -> io::Result<Value> {
    let value = match fs::read(path(repo)) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => empty(),
        Err(e) => return Err(e),
    };
    if value["version"] != 1 || !value["enabled"].is_boolean() || !value["counts"].is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid local metrics file",
        ));
    }
    Ok(value)
}

fn write(repo: &RepoInfo, value: &Value) -> io::Result<()> {
    let path = path(repo);
    fs::create_dir_all(path.parent().expect("metrics path has parent"))?;
    queue::atomic_write(&path, &serde_json::to_vec_pretty(value)?)
}

fn with_lock<T>(repo: &RepoInfo, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let metrics_path = path(repo);
    let parent = metrics_path.parent().expect("metrics path has parent");
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join("metrics.lock"))?;
    lock.lock_exclusive()?;
    let result = f();
    let _ = FileExt::unlock(&lock);
    result
}

fn report_value(repo: &RepoInfo, value: Value) -> Value {
    let discovered = value["counts"]["last_sync_discovered_sessions"]
        .as_u64()
        .unwrap_or(0);
    let captured = value["counts"]["last_sync_captured_sessions"]
        .as_u64()
        .unwrap_or(0)
        .min(discovered);
    let capture_coverage = (discovered > 0).then_some(captured as f64 / discovered as f64);
    json!({
        "enabled": value["enabled"],
        "local_only": true,
        "storage": path(repo),
        "counts": value["counts"],
        "capture_coverage_last_sync": capture_coverage,
    })
}

pub fn report(repo: &RepoInfo) -> io::Result<Value> {
    with_lock(repo, || Ok(report_value(repo, read(repo)?)))
}

pub fn set_enabled(repo: &RepoInfo, enabled: bool) -> io::Result<Value> {
    with_lock(repo, || {
        let mut value = read(repo)?;
        value["enabled"] = json!(enabled);
        write(repo, &value)?;
        Ok(report_value(repo, value))
    })
}

pub fn increment(repo: &RepoInfo, counter: &str, amount: u64) -> io::Result<()> {
    if !COUNTERS.contains(&counter) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown metrics counter",
        ));
    }
    if !path(repo).exists() {
        return Ok(());
    }
    with_lock(repo, || {
        let mut value = read(repo)?;
        if !value["enabled"].as_bool().unwrap_or(false) || amount == 0 {
            return Ok(());
        }
        let count = value["counts"][counter].as_u64().unwrap_or(0);
        value["counts"][counter] = json!(count.checked_add(amount).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "metrics counter overflow")
        })?);
        write(repo, &value)
    })
}

/// Set a last-sync gauge. It follows the same opt-in gate as cumulative events.
pub fn set_gauge(repo: &RepoInfo, counter: &str, value: u64) -> io::Result<()> {
    if !COUNTERS.contains(&counter) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown metrics counter",
        ));
    }
    if !path(repo).exists() {
        return Ok(());
    }
    with_lock(repo, || {
        let mut metrics = read(repo)?;
        if !metrics["enabled"].as_bool().unwrap_or(false) {
            return Ok(());
        }
        metrics["counts"][counter] = json!(value);
        write(repo, &metrics)
    })
}
