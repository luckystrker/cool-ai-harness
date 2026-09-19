//! Cross-language fixture tool used by `backend/tests/test_rust_store_contract.py`.
//!
//! Usage:
//!   cargo run -p cool-store --example fixture_tool -- write <db>
//!   cargo run -p cool-store --example fixture_tool -- read <db>
//!
//! `write` adopts an existing Python database and writes one conversation,
//! message, run and terminal event. `read` opens a Python database read-only
//! and prints the conversations/messages/runs/events as JSON.

use std::error::Error;
use std::path::{Path, PathBuf};

use cool_store::domains::conversations::{
    ConversationFilter, MessagePage, NewConversation, NewMessage,
};
use cool_store::domains::runs::{NewRun, RunFilter};
use cool_store::{LegacyStore, StoreOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments
        .next()
        .ok_or("usage: fixture_tool <write|read> <db>")?;
    let path = PathBuf::from(arguments.next().ok_or("missing database path")?);
    match command.as_str() {
        "write" => write(&path),
        "read" => read(&path),
        other => Err(format!("unknown command {other:?}").into()),
    }
}

fn write(path: &Path) -> Result<(), Box<dyn Error>> {
    let store = LegacyStore::open(path, &StoreOptions::default())?;
    let conversation = store.create_conversation(
        "local-user",
        &NewConversation {
            title: Some("Rust parity smoke".to_string()),
            model: Some("rust-model".to_string()),
            ..NewConversation::default()
        },
    )?;
    store.add_message(
        "local-user",
        conversation.id,
        &NewMessage {
            role: "user".to_string(),
            content: Some("written by rust".to_string()),
            ..NewMessage::default()
        },
    )?;
    let run = store.create_run(
        "local-user",
        conversation.id,
        &NewRun {
            model: Some("rust-model".to_string()),
            ..NewRun::default()
        },
    )?;
    store.append_run_event(
        "local-user",
        run.id,
        "run.started",
        Some(&json!({"provider": "rust"})),
    )?;
    store.finish_run(
        "local-user",
        run.id,
        "completed",
        Some(&json!({"total_tokens": 11})),
        Some(1),
        Some("stop"),
        None,
    )?;
    println!(
        "{}",
        json!({"conversation_id": conversation.id, "run_id": run.id})
    );
    Ok(())
}

fn read(path: &Path) -> Result<(), Box<dyn Error>> {
    let store = LegacyStore::open_read_only(path)?;
    let conversations = store.list_conversations("local-user", &ConversationFilter::default())?;
    let mut output = Vec::new();
    for conversation in conversations {
        let messages =
            store.list_messages("local-user", conversation.id, &MessagePage::default())?;
        let runs = store.list_runs("local-user", conversation.id, &RunFilter::default())?;
        let mut runs_json = Vec::new();
        for run in runs {
            let events = store.list_run_events("local-user", run.id, None, None)?;
            runs_json.push(json!({
                "id": run.id,
                "status": run.status,
                "model": run.model,
                "usage": run.usage,
                "started_at": run.started_at,
                "finished_at": run.finished_at,
                "events": events
                    .iter()
                    .map(|event| json!({
                        "seq": event.seq,
                        "kind": event.kind,
                        "payload": event.payload,
                    }))
                    .collect::<Vec<_>>(),
            }));
        }
        output.push(json!({
            "id": conversation.id,
            "title": conversation.title,
            "model": conversation.model,
            "messages": messages
                .iter()
                .map(|message| json!({"role": message.role, "content": message.content}))
                .collect::<Vec<_>>(),
            "runs": runs_json,
        }));
    }
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
