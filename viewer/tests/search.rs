use axum::{body::Body, http::Request};
use doclify_viewer::Viewer;
use tower::ServiceExt;

#[tokio::test]
async fn search_tracks_disk_changes_and_recovers_a_corrupt_index() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("docs/doclify")).unwrap();
    std::fs::write(tmp.path().join("docs/doclify/a.md"), "A unique needle").unwrap();
    let app = Viewer::new(tmp.path())
        .unwrap()
        .router("http://127.0.0.1:12345");
    async fn search(app: &axum::Router, q: &str) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/search?q={q}"))
                    .header("host", "127.0.0.1:12345")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        serde_json::from_slice(
            &http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap()
    }
    assert_eq!(search(&app, "needle").await[0]["path"], "a.md");
    std::fs::write(tmp.path().join("docs/doclify/a.md"), "changed").unwrap();
    assert_eq!(search(&app, "needle").await.as_array().unwrap().len(), 0);
    std::fs::write(tmp.path().join(".doclify/local/search.sqlite3"), "corrupt").unwrap();
    assert_eq!(search(&app, "changed").await[0]["path"], "a.md");
}

#[tokio::test]
async fn search_includes_latest_record_and_rejects_symlinked_local_storage() {
    let tmp = tempfile::tempdir().unwrap();
    let store = doclify_domain::Store::open(tmp.path()).unwrap();
    let record: doclify_domain::Record = serde_json::from_value(serde_json::json!({"id":"decision-1","version":1,"kind":"decision","title":"A choice","statement":"unique record phrase","rationale":"Reason","review":"proposed","implementation":"planned","experiment_outcome":null,"provenance":[],"evidence":[],"supersedes":null})).unwrap();
    store.propose(&record).unwrap();
    store
        .review("decision-1", 1, doclify_domain::ReviewState::Accepted)
        .unwrap();
    let app = Viewer::new(tmp.path())
        .unwrap()
        .router("http://127.0.0.1:12345");
    let req = || {
        Request::builder()
            .uri("/api/search?q=unique%20record")
            .header("host", "127.0.0.1:12345")
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(req()).await.unwrap();
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let hits: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["kind"], "record");
    assert_eq!(hits[0]["path"], "decision-1");
    #[cfg(unix)]
    {
        std::fs::remove_dir_all(tmp.path().join(".doclify/local")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), tmp.path().join(".doclify/local")).unwrap();
        assert_eq!(app.oneshot(req()).await.unwrap().status(), 500);
        assert!(!outside.path().join("search.sqlite3").exists());
    }
}
