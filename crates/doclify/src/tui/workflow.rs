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
    let status = crate::run(&["status".into()]).ok();
    let field_count = |key: &str| status.as_ref()?.get(key)?.as_array().map(Vec::len);
    let field_usize = |key: &str| status.as_ref()?.get(key)?.as_u64().map(|n| n as usize);
    let model = status
        .as_ref()
        .and_then(|v| v.get("selected_model"))
        .and_then(model_label);
    let docs = repo.worktree.join("docs/doclify");
    let documentation_files = match count_docs(&docs) {
        Ok(n) => Some(n),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(0),
        Err(e) => return Err(e),
    };
    let error_text = |name: &str| match fs::read_to_string(repo.git_dir.join("doclify").join(name))
    {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => None,
    };
    Ok(WorkflowSnapshot {
        repository: repo.worktree.display().to_string(),
        branch: repo.branch.clone(),
        model,
        registration: if status
            .as_ref()
            .and_then(|v| v.get("registered"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| repo.registered())
        {
            RegistrationState::Registered
        } else {
            RegistrationState::NotRegistered
        },
        pending_capture_events: field_usize("pending_events"),
        pending_extraction: field_usize("pending_extraction"),
        pending_review: field_count("pending_review"),
        documentation_files,
        background_error: error_text("worker-error"),
        recovery_error: error_text("recovery-error"),
    })
}

fn model_label(value: &Value) -> Option<String> {
    value
        .get("model")
        .or_else(|| value.get("provider"))
        .and_then(Value::as_str)
        .map(str::to_owned)
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
