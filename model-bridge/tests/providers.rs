use doclify_model_bridge::providers::{validate_proposals, ProposalBatch};

#[test]
fn rejects_model_claims_of_acceptance_and_missing_evidence() {
    let accepted = r#"{"proposals":[{"kind":"decision","summary":"Use guest checkout","review":"accepted","evidence":["event:1"]}]}"#;
    assert!(validate_proposals(accepted).is_err());
    let missing = r#"{"proposals":[{"kind":"decision","summary":"Use guest checkout","review":"proposed","evidence":[]}]}"#;
    assert!(validate_proposals(missing).is_err());
}

#[test]
fn accepts_only_cited_proposals() {
    let input = r#"{"proposals":[{"kind":"requirement","summary":"Allow guests","review":"proposed","evidence":["event:1"]}]}"#;
    let result: ProposalBatch = validate_proposals(input).unwrap();
    assert_eq!(result.proposals.len(), 1);
}

#[test]
fn rejects_unknown_experiment_outcomes_and_outcomes_on_other_kinds() {
    let unknown = r#"{"proposals":[{"kind":"experiment","summary":"Try bypass","review":"proposed","evidence":["event:1"],"experiment_outcome":"paused"}]}"#;
    let wrong_kind = r#"{"proposals":[{"kind":"decision","summary":"Allow bypass","review":"proposed","evidence":["event:1"],"experiment_outcome":"active"}]}"#;
    assert!(validate_proposals(unknown).is_err());
    assert!(validate_proposals(wrong_kind).is_err());
}

fn server(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0; 8192];
        let n = stream.read(&mut buf).unwrap();
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    (format!("http://{addr}/v1"), thread)
}

#[test]
fn discovers_provider_models_using_configured_key() {
    use doclify_model_bridge::providers::{ApiKind, ApiProvider};
    std::env::set_var("DOCLIFY_TEST_DISCOVERY_KEY", "fixture-key");
    let (base_url, server) = server("200 OK", r#"{"data":[{"id":"example-model"}]}"#);
    let provider = ApiProvider {
        kind: ApiKind::OpenAiCompatible,
        base_url,
        api_key_env: "DOCLIFY_TEST_DISCOVERY_KEY".into(),
        models: vec![],
    };
    assert_eq!(provider.discover().unwrap(), ["example-model"]);
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /v1/models "));
    assert!(request
        .to_lowercase()
        .contains("authorization: bearer fixture-key"));
}

#[test]
fn provider_failure_is_reported_without_exposing_body_or_key() {
    use doclify_model_bridge::providers::{ApiKind, ApiProvider};
    std::env::set_var("DOCLIFY_TEST_FAILURE_KEY", "fixture-secret");
    let (base_url, server) = server("429 Too Many Requests", "sensitive provider details");
    let provider = ApiProvider {
        kind: ApiKind::Anthropic,
        base_url,
        api_key_env: "DOCLIFY_TEST_FAILURE_KEY".into(),
        models: vec!["example".into()],
    };
    let error = provider.extract("example", "captured input").unwrap_err();
    assert!(error.contains("429"));
    assert!(!error.contains("sensitive"));
    assert!(!error.contains("fixture-secret"));
    server.join().unwrap();
}
