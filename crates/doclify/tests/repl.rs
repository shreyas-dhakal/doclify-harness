use doclify::repl::{filter_models, picker_models, request, ModelStatus};
use serde_json::json;

#[test]
fn model_search_is_case_insensitive_and_returns_provider_names() {
    assert_eq!(
        filter_models(&["alpha".into(), "Beta-2".into()], "ETA"),
        vec!["Beta-2"]
    );
}

#[test]
fn picker_retains_configured_model_and_marks_failed_discovery_unavailable() {
    let config = json!({"provider":"openai", "model":"configured-model"});
    let models = picker_models(Some(&config), Err("connection refused".into()));
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "configured-model");
    assert_eq!(models[0].status, ModelStatus::DiscoveryUnavailable);
}

#[test]
fn missing_provider_is_reported_without_running_a_model() {
    assert!(request(&json!({}), "What changed?", "context").is_err());
}

#[test]
fn api_request_returns_proposals_without_writing_or_accepting_them() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = [0; 16384];
        let _ = stream.read(&mut bytes).unwrap();
        let content = json!({"answer":"A proposal is ready.","proposals":[{"kind":"decision","summary":"Use the new flow","review":"proposed","evidence":["event:1"]}]}).to_string();
        let body = json!({"choices":[{"message":{"content":content}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    std::env::set_var("DOCLIFY_REPL_TEST_KEY", "test");
    let config = json!({"provider":"openai","base_url":format!("http://{addr}/v1"),"api_key_env":"DOCLIFY_REPL_TEST_KEY","model":"test"});
    let response = request(&config, "Propose a change", "event:1: new flow discussed").unwrap();
    assert_eq!(response.proposals[0].review, "proposed");
    worker.join().unwrap();
}
