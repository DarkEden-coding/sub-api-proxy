use crate::control::{Control, ControlEvent};
use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::{Stream, stream};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{
    sync::{broadcast, mpsc},
    time::{Instant, sleep_until},
};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);
const SUBSCRIBER_CAPACITY: usize = 32;

pub(super) async fn subscribe(
    State(control): State<Arc<dyn Control>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Subscribe before reading the initial view so a concurrent change is not lost.
    let mut changes = control.subscribe();
    let (sender, receiver) = mpsc::channel(SUBSCRIBER_CAPACITY);
    tokio::spawn(async move {
        if sender.try_send(snapshot(control.as_ref())).is_err() {
            return;
        }
        let mut next_snapshot = Instant::now() + SNAPSHOT_INTERVAL;
        let mut dirty = false;
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                _ = sleep_until(next_snapshot), if dirty => {
                    if sender.try_send(snapshot(control.as_ref())).is_err() { break; }
                    dirty = false;
                    next_snapshot = Instant::now() + SNAPSHOT_INTERVAL;
                }
                change = changes.recv() => match change {
                    Ok(ControlEvent::Changed) => { dirty = true; }
                    Ok(ControlEvent::Event(event)) => {
                        let Ok(event) = Event::default().event("event").json_data(event) else { break; };
                        if sender.try_send(Ok(event)).is_err() { break; }
                    }
                    // A lagged subscriber has lost events; disconnect so the
                    // dashboard reconnects and obtains a complete fresh view.
                    Err(broadcast::error::RecvError::Lagged(_) | broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
    let events = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|event| (event, receiver))
    });
    Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn snapshot(control: &dyn Control) -> Result<Event, Infallible> {
    // sap-core's view types contain only JSON-serializable values.
    Ok(Event::default()
        .event("snapshot")
        .json_data(control.flow())
        .expect("serializable flow snapshot"))
}
