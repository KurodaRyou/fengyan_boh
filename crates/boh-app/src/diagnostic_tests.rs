//! Capture the real request diagnostic exit, including a cancelled caller.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use axum::http::Method;
use boh_domain::AggregateId;
use boh_storage::StorageError;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::fmt::MakeWriter;

use super::{assert_internal_error, create, node, send};
use crate::actor::{Actor, Role};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_ansi(false)
            .without_time()
            .finish()
    }
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

async fn fail_equipment_insert(node: &super::Node) {
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch(
                "CREATE TRIGGER injected_failure BEFORE INSERT ON equipment
            BEGIN SELECT RAISE(ABORT, 'deterministic equipment insert failure'); END;",
            )
            .map_err(|error| StorageError::sqlite("安装测试故障触发器", error))?;
            Ok(())
        })
        .await
        .unwrap();
}

fn assert_diagnostic(capture: &Capture, command_id: &str) {
    let text = capture.text();
    assert_eq!(
        text.lines().filter(|line| line.contains("ERROR")).count(),
        1,
        "{text}"
    );
    for fragment in [
        "插入设备",
        "equipment.create",
        command_id,
        "01890a5d-ac96-774b-bcce-b302099a8101",
        "01890a5d-ac96-774b-bcce-b302099a8201",
        "deterministic equipment insert failure",
        "caused by",
        "location=",
        "projections.rs",
    ] {
        assert!(text.contains(fragment), "missing {fragment}: {text}");
    }
    for field in ["command_type=", "command_id=", "actor_id=", "device_id="] {
        assert!(text.contains(field), "{text}");
    }
    assert!(!text.contains("BUSINESS_PAYLOAD_DO_NOT_LOG"), "{text}");
}

#[tokio::test]
async fn internal_failure_logs_once_with_identity_and_hides_business_fields_and_diagnostics() {
    let node = node(true).await;
    fail_equipment_insert(&node).await;
    let capture = Capture::default();
    let mut command = create(401, "F401");
    command["name"] = "BUSINESS_PAYLOAD_DO_NOT_LOG".into();
    let id = command["command_id"].as_str().unwrap().to_owned();
    let response = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        command,
    )
    .with_subscriber(capture.subscriber())
    .await;
    assert_internal_error(response);
    assert_diagnostic(&capture, &id);
    assert_eq!(super::counts(&node).await, (0, 0, 0));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn enqueued_failure_still_logs_once_after_the_caller_drops_its_future() {
    let node = node(true).await;
    fail_equipment_insert(&node).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut blocking = Box::pin(
        node.storage
            .writer
            .call(move |_tx| -> Result<(), StorageError> {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            }),
    );
    std::future::poll_fn(|cx| {
        assert!(blocking.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    entered_rx.await.unwrap();
    let state = crate::AppState {
        writer: node.storage.writer.clone(),
        readers: node.storage.readers.clone(),
        clock: node.clock.clock(),
        timezone: boh_domain::time::parse_timezone("Asia/Shanghai").unwrap(),
        business_day_cutoff: boh_domain::time::parse_business_day_cutoff("04:00").unwrap(),
        dev_actor_stub: true,
        db_path: node._dir.path().join("boh.db"),
        backup_health: Default::default(),
    };
    let actor = Actor {
        employee_id: AggregateId::parse("01890a5d-ac96-774b-bcce-b302099a8101").unwrap(),
        device_id: AggregateId::parse("01890a5d-ac96-774b-bcce-b302099a8201").unwrap(),
        role: Role::Manager,
    };
    let mut command = create(402, "F402");
    command["name"] = "BUSINESS_PAYLOAD_DO_NOT_LOG".into();
    let id = command["command_id"].as_str().unwrap().to_owned();
    let capture = Capture::default();
    let mut request = Box::pin(
        crate::service::create(state, actor, serde_json::from_value(command).unwrap())
            .with_subscriber(capture.subscriber()),
    );
    // There is no earlier await in create: this poll enqueues the write and waits for its reply.
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(capture.text().is_empty());
    drop(request);
    release_tx.send(()).unwrap();
    blocking.await.unwrap();
    // The serial queue proves the cancelled request has finished before inspecting logs.
    node.storage
        .writer
        .call(|_tx| -> Result<(), StorageError> { Ok(()) })
        .await
        .unwrap();
    assert_diagnostic(&capture, &id);
    assert_eq!(super::counts(&node).await, (0, 0, 0));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn queue_failure_is_logged_once_on_the_request_side_with_identity() {
    let node = node(true).await;
    node.storage.writer_handle.shutdown().await.unwrap();
    let capture = Capture::default();
    let command = create(403, "F403");
    let id = command["command_id"].as_str().unwrap().to_owned();
    let response = send(node.router, Method::POST, "/api/v1/equipment", command)
        .with_subscriber(capture.subscriber())
        .await;
    assert_internal_error(response);
    let text = capture.text();
    assert_eq!(
        text.lines().filter(|line| line.contains("ERROR")).count(),
        1,
        "{text}"
    );
    for fragment in [
        "equipment.create",
        &id,
        "actor_id=",
        "device_id=",
        "writer thread is closed",
    ] {
        assert!(text.contains(fragment), "{text}");
    }
}

#[test]
fn backup_health_logs_one_full_diagnostic_with_phase_and_number() {
    use boh_domain::UnixMillis;
    use boh_storage::BackupStage;
    use boh_storage::backup::BackupHealth;
    let capture = Capture::default();
    let dir = tempfile::tempdir().unwrap();
    let cause = std::fs::File::open(dir.path().join("absent-directory")).unwrap_err();
    let error = StorageError::backup(
        BackupStage::DirectorySync,
        Some(7),
        StorageError::io("同步备份目录", cause),
    );
    let health = BackupHealth::default();
    tracing::subscriber::with_default(capture.subscriber(), || {
        health.record(&Err(error), UnixMillis(123));
    });
    let text = capture.text();
    assert_eq!(
        text.lines().filter(|line| line.contains("ERROR")).count(),
        1,
        "{text}"
    );
    for fragment in [
        "DirectorySync",
        "backup_stage=",
        "backup_number=7",
        "同步备份目录",
        "caused by",
        "diagnostic_tests.rs",
    ] {
        assert!(text.contains(fragment), "missing {fragment}: {text}");
    }
    assert_eq!(health.snapshot().last_failed_at, Some(UnixMillis(123)));
}
