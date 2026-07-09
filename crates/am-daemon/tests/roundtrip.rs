//! End-to-end test: start the daemon in-process over a real localhost socket,
//! connect a client, and verify authenticated RPC round-trips, error
//! propagation, full-text search across the socket, and live event streaming.

use std::time::Duration;

use am_core::AppCore;
use am_daemon::protocol::{DaemonRequest, DaemonResponse};
use am_daemon::{DaemonClient, Server};
use am_proto::{AppEvent, NewKnowledgeDoc, NewProject, NewTask, TaskPriority};

async fn start_daemon() -> (Server, String, std::net::SocketAddr) {
    // Unique temp data dir per run (matches the repo's existing test convention).
    let dir = std::env::temp_dir().join(format!(
        "am-daemon-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let core = AppCore::new(&dir).await.unwrap();
    let token = am_daemon::generate_token();
    let server = Server::bind(core, token.clone(), 0).await.unwrap();
    let addr = server.addr();
    (server, token, addr)
}

#[tokio::test]
async fn rejects_bad_token() {
    let (server, _token, addr) = start_daemon().await;
    let handle = tokio::spawn(server.serve());

    let result = DaemonClient::connect(addr, "not-the-token").await;
    assert!(result.is_err(), "connect should fail with a wrong token");

    handle.abort();
}

#[tokio::test]
async fn rpc_roundtrip_and_events() {
    let (server, token, addr) = start_daemon().await;
    let handle = tokio::spawn(server.serve());

    let client = DaemonClient::connect(addr, &token).await.unwrap();
    client.ping().await.unwrap();

    // Subscribe before mutating so we catch the broadcast.
    let mut events = client.subscribe_events();

    // Create a project over the socket.
    let project = client
        .create_project(NewProject {
            name: "Daemon".into(),
            description: None,
        })
        .await
        .unwrap();
    assert_eq!(project.name, "Daemon");

    // It is visible via a fresh list call.
    let projects = client.list_projects().await.unwrap();
    assert!(projects.iter().any(|p| p.id == project.id));

    // A live event for the creation should arrive.
    let mut saw_project = false;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            Ok(Ok(AppEvent::ProjectCreated(p))) if p.id == project.id => {
                saw_project = true;
                break;
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(
        saw_project,
        "expected a ProjectCreated event over the socket"
    );

    // Create a task + a doc, then search across them through the daemon.
    client
        .request(DaemonRequest::CreateTask(NewTask {
            project_id: project.id.clone(),
            title: "Wire authentication".into(),
            repo_id: None,
            description: None,
            priority: TaskPriority::Medium,
            primary_agent: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    client
        .request(DaemonRequest::CreateKnowledgeDoc(NewKnowledgeDoc {
            project_id: project.id.clone(),
            title: "Auth notes".into(),
            body: "Tokens authenticate the socket.".into(),
        }))
        .await
        .unwrap();

    let hits = client
        .search("auth", Some(&project.id), Some(20))
        .await
        .unwrap();
    let kinds: Vec<&str> = hits.iter().map(|h| h.kind.as_str()).collect();
    assert!(kinds.contains(&"task"), "search kinds: {kinds:?}");
    assert!(kinds.contains(&"doc"), "search kinds: {kinds:?}");

    // Errors propagate as a Server error, not a panic.
    let err = client
        .request(DaemonRequest::UpdateTask {
            id: "does-not-exist".into(),
            patch: Default::default(),
        })
        .await;
    assert!(matches!(err, Err(am_daemon::ClientError::Server(_))));

    // GetProject on a missing id returns a typed `None`, not an error.
    let missing = client
        .request(DaemonRequest::GetProject { id: "nope".into() })
        .await
        .unwrap();
    assert!(matches!(missing, DaemonResponse::ProjectOpt(None)));

    handle.abort();
}
