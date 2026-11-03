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
