use crate::proxy::{Body, full};
use http::{Response, StatusCode};
use tokio::sync::watch;

pub fn overloaded() -> Response<Body> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "application/json")
        .body(full(r#"{"type":"error","error":{"type":"overloaded_error","message":"Server unavailable; retry through the local router"}}"#))
        .unwrap()
}

// A generation remembers a Down transition even when recovery happens before a task wakes.
pub async fn down(receiver: &mut watch::Receiver<u64>, generation: u64) {
    loop {
        if *receiver.borrow_and_update() != generation {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
