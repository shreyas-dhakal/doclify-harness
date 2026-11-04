use std::io;

use doclify_capture::RepoInfo;

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
    Ok(WorkflowSnapshot {
        repository: repo.worktree.display().to_string(),
        branch: repo.branch.clone(),
        model: None,
        registration: if repo.registered() {
            RegistrationState::Registered
        } else {
            RegistrationState::NotRegistered
        },
        pending_capture_events: None,
        pending_extraction: None,
        pending_review: None,
        documentation_files: None,
        background_error: None,
        recovery_error: None,
    })
}
