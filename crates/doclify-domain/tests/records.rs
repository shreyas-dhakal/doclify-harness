use doclify_domain::*;

fn proposal() -> Record {
    Record {
        id: "guest-checkout".into(),
        version: 1,
        kind: RecordKind::Decision,
        title: "Allow guest checkout".into(),
        statement: "Customers may check out without an account".into(),
        rationale: "Account creation causes abandonment".into(),
        review: ReviewState::Proposed,
        implementation: ImplementationState::Planned,
        experiment_outcome: None,
        provenance: vec![Provenance {
            source: "codex:session-1".into(),
            excerpt: Some("Customers abandon account creation".into()),
        }],
        evidence: vec!["evidence:session-1".into()],
        supersedes: None,
        scope: None,
        session: None,
    }
}

#[test]
fn accepted_version_survives_a_later_proposal_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = proposal();
    store.propose(&first).unwrap();
    let accepted = store.review(&first.id, 1, ReviewState::Accepted).unwrap();
    let mut next = accepted.clone();
    next.version += 1;
    next.statement = "Guest checkout requires email".into();
    next.review = ReviewState::Proposed;
    next.supersedes = Some(accepted.version);
    store.propose(&next).unwrap();
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.accepted(&first.id).unwrap().unwrap(), accepted);
    assert_eq!(reopened.history(&first.id).unwrap().len(), 3);
}

#[test]
fn only_explicit_review_can_accept_and_history_is_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut record = proposal();
    record.review = ReviewState::Accepted;
    assert!(store.propose(&record).is_err());
    record.review = ReviewState::Proposed;
    store.propose(&record).unwrap();
    assert!(store.propose(&record).is_err());
    assert!(store.review(&record.id, 1, ReviewState::Proposed).is_err());
    assert!(store.accepted(&record.id).unwrap().is_none());
}

#[test]
fn implemented_experiment_is_not_an_accepted_decision() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut experiment = proposal();
    experiment.kind = RecordKind::Experiment;
    experiment.implementation = ImplementationState::Implemented;
    experiment.experiment_outcome = Some(ExperimentOutcome::Abandoned);
    store.propose(&experiment).unwrap();
    assert!(store.accepted(&experiment.id).unwrap().is_none());
    assert_eq!(store.history(&experiment.id).unwrap()[0], experiment);
}

#[test]
fn migrates_legacy_bare_records_and_rejects_future_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let folder = dir.path().join(".doclify/records/guest-checkout");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("00000000000000000001.json");
    std::fs::write(&path, serde_json::to_vec(&proposal()).unwrap()).unwrap();
    assert_eq!(store.history("guest-checkout").unwrap(), vec![proposal()]);
    std::fs::write(path, r#"{"schema_version":99,"record":{}}"#).unwrap();
    assert!(store.history("guest-checkout").is_err());
}

#[test]
fn reads_pre_experiment_outcome_records_with_unknown_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(".doclify/records/guest-checkout");
    std::fs::create_dir_all(&folder).unwrap();
    let mut old = serde_json::to_value(proposal()).unwrap();
    old.as_object_mut().unwrap().remove("experiment_outcome");
    std::fs::write(
        folder.join("00000000000000000001.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();

    let store = Store::open(dir.path()).unwrap();
    let history = store.history("guest-checkout").unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].experiment_outcome, None);
}

#[test]
fn fixture_keeps_requirement_decision_and_abandoned_experiment_distinct() {
    let store = Store::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/guest-checkout"
    ))
    .unwrap();
    let requirement = store
        .accepted("requirement-guest-checkout")
        .unwrap()
        .unwrap();
    assert_eq!(requirement.kind, RecordKind::Requirement);
    assert_eq!(requirement.implementation, ImplementationState::Partial);
    assert!(!requirement.provenance[0]
        .excerpt
        .as_ref()
        .unwrap()
        .is_empty());
    let decision = store.accepted("decision-guest-checkout").unwrap().unwrap();
    assert_eq!(decision.statement, "Allow guest checkout.");
    assert_eq!(
        store
            .history("decision-guest-checkout")
            .unwrap()
            .last()
            .unwrap()
            .review,
        ReviewState::Proposed
    );
    let experiment = store
        .history("experiment-account-bypass")
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        experiment.experiment_outcome,
        Some(ExperimentOutcome::Abandoned)
    );
    assert_eq!(experiment.implementation, ImplementationState::Implemented);
}

#[test]
fn concurrent_appends_cannot_overwrite_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || Store::open(root).unwrap().propose(&proposal()))
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        Store::open(root)
            .unwrap()
            .history("guest-checkout")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reviewed_reversion_and_supersession_preserve_prior_versions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.propose(&proposal()).unwrap();
    let accepted = store
        .review("guest-checkout", 1, ReviewState::Accepted)
        .unwrap();
    let mut revision = accepted.clone();
    revision.version = 3;
    revision.review = ReviewState::Proposed;
    revision.implementation = ImplementationState::Reverted;
    revision.supersedes = Some(accepted.version);
    store.propose(&revision).unwrap();
    assert_eq!(
        store
            .accepted(&revision.id)
            .unwrap()
            .unwrap()
            .implementation,
        ImplementationState::Planned
    );
    let reviewed = store
        .review(&revision.id, 3, ReviewState::Accepted)
        .unwrap();
    assert_eq!(reviewed.implementation, ImplementationState::Reverted);
    assert_eq!(store.history(&revision.id).unwrap()[1], accepted);
    assert_eq!(store.accepted(&revision.id).unwrap().unwrap(), reviewed);
}

#[test]
fn old_records_without_scope_or_session_deserialize() {
    let mut value = serde_json::to_value(proposal()).unwrap();
    value.as_object_mut().unwrap().remove("scope");
    value.as_object_mut().unwrap().remove("session");
    let restored: Record = serde_json::from_value(value).unwrap();
    assert_eq!(restored.scope, None);
    assert_eq!(restored.session, None);
}
