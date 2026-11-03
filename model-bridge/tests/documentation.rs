use doclify_model_bridge::documentation::{schema, validate_response};
use doclify_model_bridge::providers::ProposalBatch;

#[test]
fn validates_answers_and_proposed_edits() {
    let value = r#"{"answer":"Guests may check out.","proposals":[{"kind":"requirement","summary":"Allow guests","review":"proposed","evidence":["event:1"]}]}"#;
    let result = validate_response(value).unwrap();
    assert_eq!(result.answer, "Guests may check out.");
    assert_eq!(
        result.proposals,
        ProposalBatch {
            proposals: vec![doclify_model_bridge::providers::Proposal {
                kind: "requirement".into(),
                summary: "Allow guests".into(),
                review: "proposed".into(),
                evidence: vec!["event:1".into()]
            }]
        }
        .proposals
    );
    assert!(schema().is_object());
}

#[test]
fn rejects_acceptance_empty_answer_and_extra_fields() {
    for value in [
        r#"{"answer":"ok","proposals":[{"kind":"decision","summary":"x","review":"accepted","evidence":["e"]}]}"#,
        r#"{"answer":" ","proposals":[]}"#,
        r#"{"answer":"ok","proposals":[],"extra":true}"#,
    ] {
        assert!(validate_response(value).is_err());
    }
}
