//! Portable, append-only documentation records. Opening a store never accepts a proposal.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Requirement,
    Decision,
    Experiment,
    Question,
    Task,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Proposed,
    Accepted,
    Rejected,
    Superseded,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationState {
    Unknown,
    Planned,
    Partial,
    Implemented,
    Reverted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentOutcome {
    Active,
    Completed,
    Abandoned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub excerpt: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub version: u64,
    pub kind: RecordKind,
    pub title: String,
    pub statement: String,
    pub rationale: String,
    pub review: ReviewState,
    pub implementation: ImplementationState,
    pub experiment_outcome: Option<ExperimentOutcome>,
    pub provenance: Vec<Provenance>,
    pub evidence: Vec<String>,
    pub supersedes: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    schema_version: u32,
    record: Record,
}

/// The argument to `open` is a repository root; records live under `.doclify/records`.
pub struct Store {
    root: PathBuf,
}
impl Store {
    pub fn open(repository: impl AsRef<Path>) -> io::Result<Self> {
        let root = repository.as_ref().join(".doclify/records");
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
    fn directory(&self, id: &str) -> io::Result<PathBuf> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(invalid(
                "record id must contain only ASCII letters, digits, '-' or '_'",
            ));
        }
        Ok(self.root.join(id))
    }
    pub fn history(&self, id: &str) -> io::Result<Vec<Record>> {
        let path = self.directory(id)?;
        let entries = match fs::read_dir(path) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_reader(File::open(entry.path())?).map_err(data)?;
            let envelope = migrate(value)?;
            if envelope.record.id != id
                || entry.file_name().to_string_lossy()
                    != format!("{:020}.json", envelope.record.version)
            {
                return Err(invalid("record path does not match its identity"));
            }
            records.push(envelope.record);
        }
        records.sort_by_key(|r| r.version);
        if records
            .iter()
            .enumerate()
            .any(|(i, r)| r.version != i as u64 + 1)
        {
            return Err(invalid("record history has missing versions"));
        }
        Ok(records)
    }
    /// Return the latest version of each record in deterministic order.
    pub fn latest_records(&self) -> io::Result<Vec<Record>> {
        let mut ids = fs::read_dir(&self.root)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        ids.sort();
        let mut records = Vec::new();
        for id in ids {
            let id = id
                .to_str()
                .ok_or_else(|| invalid("invalid record directory"))?;
            if self.root.join(id).is_dir() {
                if let Some(record) = self.history(id)?.pop() {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }
    pub fn accepted(&self, id: &str) -> io::Result<Option<Record>> {
        let mut accepted: Option<Record> = None;
        for record in self.history(id)? {
            match record.review {
                ReviewState::Accepted => accepted = Some(record),
                ReviewState::Superseded
                    if accepted
                        .as_ref()
                        .is_some_and(|r| Some(r.version) == record.supersedes) =>
                {
                    accepted = None
                }
                _ => (),
            }
        }
        Ok(accepted)
    }
    pub fn propose(&self, record: &Record) -> io::Result<()> {
        if record.review != ReviewState::Proposed {
            return Err(invalid("new records must be proposed"));
        }
        let history = self.history(&record.id)?;
        if record.version != history.len() as u64 + 1 {
            return Err(invalid("version must follow existing history"));
        }
        if let Some(version) = record.supersedes {
            if version == 0 || version >= record.version {
                return Err(invalid("invalid superseded version"));
            }
        }
        self.append(record)
    }
    /// Review is explicit and appends a new immutable version of the proposal.
    pub fn review(
        &self,
        id: &str,
        proposal_version: u64,
        state: ReviewState,
    ) -> io::Result<Record> {
        if !matches!(
            state,
            ReviewState::Accepted | ReviewState::Rejected | ReviewState::Superseded
        ) {
            return Err(invalid("review must resolve a proposal"));
        }
        let history = self.history(id)?;
        let mut record = history
            .last()
            .filter(|r| r.version == proposal_version && r.review == ReviewState::Proposed)
            .cloned()
            .ok_or_else(|| invalid("review target is not the latest proposal"))?;
        record.version += 1;
        record.review = state;
        self.append(&record)?;
        Ok(record)
    }
    fn append(&self, record: &Record) -> io::Result<()> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let dir = self.directory(&record.id)?;
        fs::create_dir_all(&dir)?;
        let temp = dir.join(format!(
            ".{}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            serde_json::to_writer_pretty(
                &mut file,
                &Envelope {
                    schema_version: 1,
                    record: record.clone(),
                },
            )
            .map_err(data)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::hard_link(&temp, dir.join(format!("{:020}.json", record.version)))?;
            File::open(&dir)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(temp);
        result
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
/// Version 0 stored a bare record. Version 1 wraps it in a schema-tagged envelope.
fn migrate(value: serde_json::Value) -> io::Result<Envelope> {
    match value.get("schema_version").and_then(|v| v.as_u64()) {
        None if value.get("id").is_some() => Ok(Envelope {
            schema_version: 1,
            record: serde_json::from_value(value).map_err(data)?,
        }),
        Some(1) => serde_json::from_value(value).map_err(data),
        _ => Err(invalid("unsupported record schema version")),
    }
}
