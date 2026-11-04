use std::{fs, io, path::Path};

use doclify_capture::RepoInfo;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationState {
    Registered,
    NotRegistered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSnapshot {
    pub repository: String,
    pub branch: Option<String>,
    /// Provider name when selected, or configured API model ID.
    pub model: Option<String>,
    pub registration: RegistrationState,
    pub pending_capture_events: Option<usize>,
    pub pending_extraction: Option<usize>,
    pub pending_review: Option<usize>,
    pub documentation_files: Option<usize>,
    pub background_error: Option<String>,
    pub recovery_error: Option<String>,
}

pub fn load(repo: &RepoInfo) -> io::Result<WorkflowSnapshot> {
    let status = crate::run(&["status".into()])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let field_count = |key: &str| -> io::Result<Option<usize>> {
        match status.get(key) {
            None => Ok(None),
            Some(value) => value
                .as_array()
                .map(|items| Some(items.len()))
                .ok_or_else(|| invalid_status_field(key)),
        }
    };
    let field_usize = |key: &str| -> io::Result<Option<usize>> {
        match status.get(key) {
            None => Ok(None),
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| invalid_status_field(key)),
        }
    };
    let model = match status.get("selected_model") {
        None | Some(Value::Null) => None,
        Some(value) => Some(model_label(value)?),
    }
    .flatten();
    let docs = repo.worktree.join("docs/doclify");
    let documentation_files = match count_docs(&docs) {
        Ok(n) => Some(n),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(0),
        Err(e) => return Err(e),
    };
    let background_error = read_optional(repo.git_dir.join("doclify/worker-error"))?;
    let recovery_error = read_optional(repo.git_dir.join("doclify/recovery-error"))?;
    let registration = match status.get("registered") {
        None => repo.registered(),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid_status_field("registered"))?,
    };
    Ok(WorkflowSnapshot {
        repository: repo.worktree.display().to_string(),
        branch: repo.branch.clone(),
        model,
        registration: if registration {
            RegistrationState::Registered
        } else {
            RegistrationState::NotRegistered
        },
        pending_capture_events: field_usize("pending_events")?,
        pending_extraction: field_usize("pending_extraction")?,
        pending_review: field_count("pending_review")?,
        documentation_files,
        background_error,
        recovery_error,
    })
}

fn invalid_status_field(field: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("status field {field:?} has an invalid value"),
    )
}

fn model_label(value: &Value) -> io::Result<Option<String>> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_status_field("selected_model"))?;
    let model = object.get("model").or_else(|| object.get("provider"));
    match model {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(label)) => Ok(Some(label.clone())),
        Some(_) => Err(invalid_status_field("selected_model")),
    }
}

fn read_optional(path: impl AsRef<Path>) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn count_docs(dir: &Path) -> io::Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            count += count_docs(&entry.path())?;
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("mdx")
                })
        {
            count += 1;
        }
    }
    Ok(count)
}
