use cool_app_server::{AppServer, ServerConfig};
use cool_protocol::{ActorKind, ActorRef, CanonicalEvent, EventEnvelope, RunStarted, V1Version};
use cool_state::DurableStore;
use tempfile::tempdir;

#[tokio::test]
async fn app_server_restart_recovers_and_replays_an_interrupted_run() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("rust-core.db");
    let (session_id, run_id) = {
        let store = DurableStore::open(&path).unwrap();
        let session_id = store
            .create_session("local-user", "session", "session-fp", None, None)
            .unwrap()
            .value;
        let run_id = store
            .start_run("local-user", "prompt", "prompt-fp", &session_id)
            .unwrap()
            .value;
        store
            .append_event(
                "local-user",
                &EventEnvelope {
                    event_id: "event-before-crash".to_owned(),
                    schema_version: V1Version::VALUE,
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    item_id: None,
                    seq: 1,
                    occurred_at: "2026-09-01T00:00:00Z".to_owned(),
                    actor: ActorRef {
                        id: "python-adapter".to_owned(),
                        kind: ActorKind::Worker,
                    },
                    source: "test-worker".to_owned(),
                    causation_id: None,
                    correlation_id: None,
                    event: CanonicalEvent::RunStarted(RunStarted {
                        model: None,
                        mode: Some("python-adapter".to_owned()),
                    }),
                    extensions: Default::default(),
                },
            )
            .unwrap();
        (session_id, run_id)
    };

    let reopened = DurableStore::open(&path).unwrap();
    let server = AppServer::with_store(ServerConfig::default(), reopened).unwrap();
    let events = server.events_for_run(&run_id).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].actor.kind, ActorKind::Worker);
    assert_eq!(events[0].actor.id, "python-adapter");
    assert!(matches!(events[1].event, CanonicalEvent::RunFailed(_)));
    assert_eq!(events[1].actor.kind, ActorKind::System);
    assert_eq!(events[1].actor.id, "cool-core");
    let session = server
        .store()
        .load_session(&session_id, "local-user")
        .unwrap();
    assert_eq!(session.active_run_id, None);
}
