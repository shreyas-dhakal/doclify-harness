//! Retryable extraction of durable capture events into local proposal payloads.
//! Consumers must validate proposals against their domain schema before applying them.
use doclify_capture::queue::{atomic_write, Queue};
use serde_json::Value;
use std::{fs, io, path::Path};

pub trait Extractor {
    fn extract(&mut self, event: &Value) -> io::Result<serde_json::Value>;
}

/// Process pending work serially. A provider or storage error stops processing and leaves
/// the current and remaining events pending. Persisted results are reused after a crash.
pub fn process(
    capture: &Queue,
    output: &Path,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    fs::create_dir_all(output)?;
    let mut completed = 0;
    for (id, event) in capture.pending()? {
        let path = output.join(&id);
        if path.exists() {
            let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            validate(&value)?;
        } else {
            let value = extractor.extract(&event)?;
            validate(&value)?;
            atomic_write(&path, &serde_json::to_vec(&value)?)?;
        }
        capture.ack(&id)?;
        completed += 1;
    }
    Ok(completed)
}
fn validate(value: &serde_json::Value) -> io::Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(state) = map.get("review") {
                if state.as_str() != Some("proposed") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "extractor cannot set a reviewed state",
                    ));
                }
            }
            for item in map.values() {
                validate(item)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}
