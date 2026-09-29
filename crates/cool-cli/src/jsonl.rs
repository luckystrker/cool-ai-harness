//! NDJSON event streaming for `cool run --mode json` (P2.14): every
//! `CanonicalEvent` lands on stdout as one JSON line; progress and warnings
//! stay on stderr. The `run.completed` envelope is held back and re-emitted
//! by [`JsonlEventSink::finish`] with the run's result text injected, so the
//! last line is always the terminal event.
use std::io::Write;
use std::sync::Mutex;

use async_trait::async_trait;
use cool_agent::{EventSink, RuntimeError, StoreEventSink};
use cool_agent::{Message, Usage};
use cool_protocol::{CanonicalEvent, EventEnvelope, Extensions};
use serde_json::json;

pub struct JsonlEventSink {
    inner: StoreEventSink,
    /// Serializes NDJSON writes: tool batches emit from concurrent tasks.
    out: Mutex<()>,
    /// The terminal envelope, captured in `emit` and written by `finish`
    /// once the run's result text is known.
    pending_completed: Mutex<Option<EventEnvelope>>,
}

impl JsonlEventSink {
    pub fn new(inner: StoreEventSink) -> Self {
        Self {
            inner,
            out: Mutex::new(()),
            pending_completed: Mutex::new(None),
        }
    }

    /// The shared post-emit step: hold `run.completed` back for `finish`,
    /// print every other envelope as one NDJSON line.
    fn render(&self, envelope: EventEnvelope) -> Result<EventEnvelope, RuntimeError> {
        if matches!(envelope.event, CanonicalEvent::RunCompleted(_)) {
            *self
                .pending_completed
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(envelope.clone());
        } else {
            self.write_line(&envelope);
        }
        Ok(envelope)
    }

    fn write_line(&self, envelope: &EventEnvelope) {
        let Ok(line) = serde_json::to_string(envelope) else {
            return;
        };
        let _guard = self.out.lock().unwrap_or_else(|poison| poison.into_inner());
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    /// Writes the captured `run.completed` line with the result text merged
    /// into its payload — the last NDJSON line a consumer reads.
    pub fn finish(&self, result_text: &str) {
        let completed = self
            .pending_completed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let Some(envelope) = completed else {
            return;
        };
        match serde_json::to_value(&envelope) {
            Ok(mut value) => {
                let target = value
                    .get_mut("event")
                    .and_then(|event| event.get_mut("payload"))
                    .and_then(serde_json::Value::as_object_mut);
                match target {
                    Some(target) => {
                        target.insert("result".to_owned(), json!({ "text": result_text }));
                    }
                    None => {
                        if let Some(root) = value.as_object_mut() {
                            root.insert("result".to_owned(), json!({ "text": result_text }));
                        }
                    }
                }
                if let Ok(line) = serde_json::to_string(&value) {
                    let _guard = self.out.lock().unwrap_or_else(|poison| poison.into_inner());
                    let stdout = std::io::stdout();
                    let mut stdout = stdout.lock();
                    let _ = writeln!(stdout, "{line}");
                    let _ = stdout.flush();
                }
            }
            Err(_) => self.write_line(&envelope),
        }
    }
}

#[async_trait]
impl EventSink for JsonlEventSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let envelope = self.inner.emit(event).await?;
        self.render(envelope)
    }

    async fn emit_with_extensions(
        &self,
        event: CanonicalEvent,
        extensions: Extensions,
    ) -> Result<EventEnvelope, RuntimeError> {
        // Forward the extensions — the trait default drops them, which would
        // lose checkpoint refs (P2.18) and replay parts (P2.12) from the
        // durable log the inner sink writes.
        let envelope = self.inner.emit_with_extensions(event, extensions).await?;
        self.render(envelope)
    }

    async fn before_compaction(&self, history: &[Message]) -> Result<(), RuntimeError> {
        self.inner.before_compaction(history).await
    }

    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        self.inner.load_history().await
    }

    async fn reserve_usage(&self, usage: &Usage) -> Result<(), RuntimeError> {
        self.inner.reserve_usage(usage).await
    }

    async fn drain_steers(&self) -> Result<Vec<Message>, RuntimeError> {
        self.inner.drain_steers().await
    }

    async fn summarize_for_compaction(
        &self,
        dropped: &[Message],
        retained: usize,
    ) -> Result<Option<String>, RuntimeError> {
        self.inner.summarize_for_compaction(dropped, retained).await
    }
}
