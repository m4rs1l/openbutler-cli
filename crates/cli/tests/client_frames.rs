//! The framed client still owes every frame it decoded: one socket read can
//! carry several frames, and a reader that keeps only the first drops the
//! burst the host just flushed behind it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(windows)]

use std::time::Duration;

use openbutler_cli::client::Client;
use openbutler_protocol::{DAEMON_PROTOCOL_VERSION, DaemonEvent};
use tokio::io::AsyncWriteExt;

/// A scratch data directory (the client derives its endpoint from it).
fn temp_data_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ob-cli-frames-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn notification(text: &str) -> DaemonEvent {
    DaemonEvent::Notification {
        version: DAEMON_PROTOCOL_VERSION,
        title: text.to_owned(),
        message: String::new(),
    }
}

/// The host flushes its pending frames back to back; a single read can
/// return both. Both must reach the caller - a dropped one leaves a run
/// loop waiting for a state change that already arrived.
#[tokio::test]
async fn two_frames_in_one_read_are_both_delivered() {
    let dir = temp_data_dir("coalesced");
    let name = openbutler_protocol::endpoint::endpoint_for(&dir).socket_name();
    let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
    options.first_pipe_instance(true);
    let mut server = options.create(&name).unwrap();

    let payload = {
        let mut bytes = Vec::new();
        for event in [notification("first"), notification("second")] {
            bytes.extend_from_slice(&serde_json::to_vec(&event).unwrap());
            bytes.push(b'\n');
        }
        bytes
    };
    let writer = tokio::spawn(async move {
        server.connect().await.unwrap();
        // One write: the two frames land in one buffer.
        server.write_all(&payload).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let mut client = Client::connect(&dir).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), client.next_event())
        .await
        .expect("the first frame arrives")
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), client.next_event())
        .await
        .expect("the frame behind it arrives too")
        .unwrap();
    for (event, wanted) in [(first, "first"), (second, "second")] {
        let DaemonEvent::Notification { title, .. } = event else {
            panic!("expected a notification");
        };
        assert_eq!(title, wanted);
    }

    writer.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
