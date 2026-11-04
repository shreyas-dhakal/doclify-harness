use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use doclify_viewer::Viewer;
use tower::ServiceExt;

#[tokio::test]
async fn saves_require_token_origin_and_current_revision() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("docs/doclify")).unwrap();
    std::fs::write(tmp.path().join("docs/doclify/a.md"), "old").unwrap();
    let viewer = Viewer::new(tmp.path()).unwrap();
    let app = viewer.router("http://127.0.0.1:12345");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/document?path=a.md")
                .header("host", "127.0.0.1:12345")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let save = |origin: &str, token: &str, revision: &str| {
        Request::builder()
            .method("PUT")
            .uri("/api/document")
            .header("host", "127.0.0.1:12345")
            .header("content-type", "application/json")
            .header("origin", origin)
            .header("x-doclify-token", token)
            .body(Body::from(
                serde_json::json!({"path":"a.md", "content":"new", "revision":revision})
                    .to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(save(
                "http://evil.test",
                viewer.token(),
                doc["revision"].as_str().unwrap()
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(save(
                "http://127.0.0.1:12345",
                "bad",
                doc["revision"].as_str().unwrap()
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(save(
                "http://127.0.0.1:12345",
                viewer.token(),
                doc["revision"].as_str().unwrap()
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(save(
            "http://127.0.0.1:12345",
            viewer.token(),
            doc["revision"].as_str().unwrap()
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("docs/doclify/a.md")).unwrap(),
        "new"
    );
}

#[tokio::test]
async fn review_is_explicit_and_rejects_stale_proposals() {
    let tmp = tempfile::tempdir().unwrap();
    let store = doclify_domain::Store::open(tmp.path()).unwrap();
    let record: doclify_domain::Record = serde_json::from_value(serde_json::json!({"id":"decision-1","version":1,"kind":"decision","title":"Decision","statement":"Statement","rationale":"Reason","review":"proposed","implementation":"planned","experiment_outcome":null,"provenance":[],"evidence":[],"supersedes":null})).unwrap();
    store.propose(&record).unwrap();
    let viewer = Viewer::new(tmp.path()).unwrap();
    let app = viewer.router("http://127.0.0.1:12345");
    let request = |token: &str| {
        Request::builder()
            .method("POST")
            .uri("/api/review")
            .header("host", "127.0.0.1:12345")
            .header("origin", "http://127.0.0.1:12345")
            .header("x-doclify-token", token)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"id":"decision-1","version":1,"action":"accept"}"#,
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(request("wrong"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(store.history("decision-1").unwrap().len(), 1);
    assert_eq!(
        app.clone()
            .oneshot(request(viewer.token()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(request(viewer.token())).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        store
            .accepted("decision-1")
            .unwrap()
            .unwrap()
            .implementation,
        doclify_domain::ImplementationState::Planned
    );
}

#[tokio::test]
async fn rejects_untrusted_host_and_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("docs/doclify")).unwrap();
    let app = Viewer::new(tmp.path())
        .unwrap()
        .router("http://127.0.0.1:12345");
    let req = |host: &str, uri: &str| {
        Request::builder()
            .uri(uri)
            .header("host", host)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(req("attacker.test", "/api/documents"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.oneshot(req("127.0.0.1:12345", "/api/document?path=..%2Fsecret.md"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn concurrent_saves_of_one_revision_have_one_winner_and_conflicts_return_latest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("docs/doclify")).unwrap();
    std::fs::write(tmp.path().join("docs/doclify/a.md"), "old").unwrap();
    let revision = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"old"));
    // Two viewers stand in for two processes; they share only the file lock.
    let apps = [
        Viewer::new(tmp.path()).unwrap(),
        Viewer::new(tmp.path()).unwrap(),
    ];
    let tasks = (0..8).map(|i| {
        let viewer = apps[i % 2].clone();
        let revision = revision.clone();
        tokio::spawn(async move {
            let request = Request::builder()
                .method("PUT")
                .uri("/api/document")
                .header("host", "127.0.0.1:12345")
                .header("origin", "http://127.0.0.1:12345")
                .header("x-doclify-token", viewer.token())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"path":"a.md", "content":format!("v{i}"), "revision":revision})
                        .to_string(),
                ))
                .unwrap();
            let response = viewer
                .router("http://127.0.0.1:12345")
                .oneshot(request)
                .await
                .unwrap();
            let status = response.status();
            let body = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap())
        })
    });
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    let on_disk = std::fs::read_to_string(tmp.path().join("docs/doclify/a.md")).unwrap();
    assert_eq!(results.iter().filter(|r| r.0 == StatusCode::OK).count(), 1);
    for (status, body) in results {
        assert_eq!(body["content"], on_disk.as_str());
        assert!(status == StatusCode::OK || status == StatusCode::CONFLICT);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_lead_reads_or_writes_out_of_the_docs_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.md"), "secret").unwrap();
    std::fs::create_dir_all(tmp.path().join("docs/doclify")).unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("docs/doclify/dir")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("x.md"),
        tmp.path().join("docs/doclify/file.md"),
    )
    .unwrap();
    let viewer = Viewer::new(tmp.path()).unwrap();
    let app = viewer.router("http://127.0.0.1:12345");
    for path in ["dir/x.md", "file.md"] {
        let read = Request::builder()
            .uri(format!("/api/document?path={path}"))
            .header("host", "127.0.0.1:12345")
            .body(Body::empty())
            .unwrap();
        assert!(!app
            .clone()
            .oneshot(read)
            .await
            .unwrap()
            .status()
            .is_success());
        let revision = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"secret"));
        let save = Request::builder()
            .method("PUT")
            .uri("/api/document")
            .header("host", "127.0.0.1:12345")
            .header("origin", "http://127.0.0.1:12345")
            .header("x-doclify-token", viewer.token())
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"path":path, "content":"pwned", "revision":revision})
                    .to_string(),
            ))
            .unwrap();
        assert!(!app
            .clone()
            .oneshot(save)
            .await
            .unwrap()
            .status()
            .is_success());
    }
    assert_eq!(
        std::fs::read_to_string(outside.path().join("x.md")).unwrap(),
        "secret"
    );
}

#[tokio::test]
async fn record_source_returns_exact_portable_version() {
    let tmp = tempfile::tempdir().unwrap();
    let store = doclify_domain::Store::open(tmp.path()).unwrap();
    let record: doclify_domain::Record = serde_json::from_value(serde_json::json!({"id":"decision-1","version":1,"kind":"decision","title":"Decision","statement":"Statement","rationale":"Reason","review":"proposed","implementation":"planned","experiment_outcome":null,"provenance":[{"source":"event:123","excerpt":"portable excerpt"}],"evidence":["event:123"],"supersedes":null})).unwrap();
    store.propose(&record).unwrap();
    store
        .review("decision-1", 1, doclify_domain::ReviewState::Accepted)
        .unwrap();
    let app = Viewer::new(tmp.path())
        .unwrap()
        .router("http://127.0.0.1:12345");
    let req = |uri| {
        Request::builder()
            .uri(uri)
            .header("host", "127.0.0.1:12345")
            .body(Body::empty())
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(req("/api/record-source?id=decision-1&version=1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let source: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(source["review"], "proposed");
    assert_eq!(source["provenance"][0]["excerpt"], "portable excerpt");
    assert_eq!(
        app.oneshot(req("/api/record-source?id=..%2Fsecret&version=1"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_regeneration_and_viewer_save_preserve_the_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .status()
        .unwrap()
        .success());
    doclify_evidence::docs::generate(repo, None).unwrap();
    let path = repo.join("docs/doclify/overview.mdx");
    let old = std::fs::read_to_string(&path).unwrap();
    let revision = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(old.as_bytes())
    );
    std::fs::write(repo.join("README.md"), "new file").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", "README.md"])
        .current_dir(repo)
        .status()
        .unwrap()
        .success());
    let lock = std::fs::File::open(repo.join("docs/doclify")).unwrap();
    lock.lock().unwrap();
    let generation_repo = repo.to_owned();
    let generation = tokio::task::spawn_blocking(move || {
        doclify_evidence::docs::generate(&generation_repo, None).unwrap()
    });
    let viewer = Viewer::new(repo).unwrap();
    let request = Request::builder().method("PUT").uri("/api/document").header("host", "127.0.0.1:12345").header("origin", "http://127.0.0.1:12345").header("x-doclify-token", viewer.token()).header("content-type", "application/json").body(Body::from(serde_json::json!({"path":"overview.mdx", "content":format!("{old}\nHuman change.\n"), "revision":revision}).to_string())).unwrap();
    let saving = tokio::spawn(async move {
        viewer
            .router("http://127.0.0.1:12345")
            .oneshot(request)
            .await
            .unwrap()
    });
    drop(lock);
    let report = generation.await.unwrap();
    let response = saving.await.unwrap();
    let current = std::fs::read_to_string(path).unwrap();
    match response.status() {
        StatusCode::OK => assert!(
            current.contains("Human change."),
            "successful save lost during regeneration: {report:?}"
        ),
        StatusCode::CONFLICT => {
            let body = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let latest: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(latest["content"], current);
            assert!(!current.contains("Human change."));
        }
        status => panic!("unexpected save status {status}"),
    }
}
