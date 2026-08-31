use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::model::{CommandReceipt, Event, SpikeError, SpikeResult};
use crate::store::{ApprovalFailpoint, PromptFailpoint, Store, hash_json};
use crate::worker::{WorkerMessage, WorkerRequest, spawn_worker};

#[derive(Clone, Copy, Debug, Default)]
pub struct CorePolicy {
    pub allow_write: bool,
    pub approval_failpoint: Option<ApprovalFailpoint>,
    pub prompt_failpoint: Option<PromptFailpoint>,
}

#[derive(Clone, Debug)]
pub struct SpikeCore {
    store: Store,
    worker_executable: PathBuf,
    policy: CorePolicy,
}

#[derive(Clone, Debug)]
pub struct PromptRequest {
    pub session_id: String,
    pub actor: String,
    pub idempotency_key: String,
    pub prompt: String,
    pub mode: String,
}

#[derive(Debug)]
pub struct CoreOutcome {
    pub receipt: CommandReceipt,
    pub emitted: Vec<Event>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteMarkerArgs {
    value: String,
}

impl SpikeCore {
    pub fn new(store: Store, worker_executable: impl AsRef<Path>, policy: CorePolicy) -> Self {
        Self {
            store,
            worker_executable: worker_executable.as_ref().to_path_buf(),
            policy,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub async fn prompt(
        &self,
        request: PromptRequest,
        event_sender: &mpsc::Sender<Event>,
    ) -> SpikeResult<CoreOutcome> {
        let request_hash = hash_json(&json!({
            "mode": request.mode,
            "prompt": request.prompt,
            "sessionId": request.session_id
        }))?;
        let creation = self.store.create_or_get_run(
            &request.session_id,
            &request.actor,
            &request.idempotency_key,
            &request_hash,
        )?;
        if !creation.created {
            let receipt = if let Some(receipt) = creation.receipt {
                receipt
            } else {
                let (receipt, recovered_events) = self.store.recover_incomplete_prompt(
                    &request.actor,
                    &request.idempotency_key,
                    &creation.run_id,
                )?;
                self.publish_events(event_sender, &recovered_events).await;
                receipt
            };
            return Ok(CoreOutcome {
                receipt,
                emitted: Vec::new(),
            });
        }

        let run_id = creation.run_id;
        let mut emitted = Vec::new();
        self.emit(
            &mut emitted,
            event_sender,
            &run_id,
            "run.started",
            json!({"idempotencyKey": request.idempotency_key}),
            &request.actor,
            "core",
            None,
        )
        .await?;
        self.store.increment_worker_attempts(&run_id)?;
        self.emit(
            &mut emitted,
            event_sender,
            &run_id,
            "worker.started",
            json!({"attempt": 1}),
            &request.actor,
            "supervisor",
            None,
        )
        .await?;

        let mut stream = match spawn_worker(
            &self.worker_executable,
            &WorkerRequest {
                run_id: run_id.clone(),
                prompt: request.prompt.clone(),
                mode: request.mode.clone(),
            },
        )
        .await
        {
            Ok(stream) => stream,
            Err(_) => {
                return self
                    .fail_worker(
                        &request,
                        &run_id,
                        "worker_unavailable",
                        &mut emitted,
                        event_sender,
                    )
                    .await;
            }
        };

        let mut tool_intent: Option<(String, String, Value)> = None;
        let mut worker_stream_error = false;
        while let Some(message) = stream.messages.recv().await {
            match message {
                Ok(WorkerMessage::ContentDelta { text }) => {
                    self.emit(
                        &mut emitted,
                        event_sender,
                        &run_id,
                        "content.delta",
                        json!({"text": text, "durable": true}),
                        &request.actor,
                        "scripted-worker",
                        None,
                    )
                    .await?;
                }
                Ok(WorkerMessage::ToolIntent {
                    call_id,
                    name,
                    arguments,
                }) => {
                    self.emit(
                        &mut emitted,
                        event_sender,
                        &run_id,
                        "tool.requested",
                        json!({"callId": call_id, "name": name, "arguments": arguments}),
                        &request.actor,
                        "scripted-worker",
                        Some(&call_id),
                    )
                    .await?;
                    if tool_intent.is_some() {
                        return self
                            .fail_tool(
                                &request,
                                &run_id,
                                &call_id,
                                "multiple_tool_intents",
                                &mut emitted,
                                event_sender,
                            )
                            .await;
                    }
                    if name != "write_marker" || !self.policy.allow_write {
                        return self
                            .fail_tool(
                                &request,
                                &run_id,
                                &call_id,
                                "capability_denied",
                                &mut emitted,
                                event_sender,
                            )
                            .await;
                    }
                    let validated = serde_json::from_value::<WriteMarkerArgs>(arguments.clone());
                    if !matches!(validated, Ok(args) if !args.value.is_empty()) {
                        return self
                            .fail_tool(
                                &request,
                                &run_id,
                                &call_id,
                                "invalid_tool_arguments",
                                &mut emitted,
                                event_sender,
                            )
                            .await;
                    }
                    tool_intent = Some((call_id, name, arguments));
                }
                Err(_) => worker_stream_error = true,
            }
        }
        let worker_exit = stream.completion.await.map_err(|error| {
            SpikeError::Worker(format!("worker supervision task failed: {error}"))
        })?;
        let worker_exit = match worker_exit {
            Ok(exit) if !worker_stream_error => exit,
            _ => {
                return self
                    .fail_worker(
                        &request,
                        &run_id,
                        "worker_output_rejected",
                        &mut emitted,
                        event_sender,
                    )
                    .await;
            }
        };
        if !worker_exit.success {
            return self
                .fail_worker(
                    &request,
                    &run_id,
                    &format!("worker_crashed:{:?}", worker_exit.exit_code),
                    &mut emitted,
                    event_sender,
                )
                .await;
        }

        let receipt = if let Some((call_id, name, arguments)) = tool_intent {
            let (receipt, approval_events) = self.store.prepare_approval_atomic(
                &request.actor,
                &request.idempotency_key,
                &run_id,
                &call_id,
                &name,
                &arguments,
                self.policy.prompt_failpoint,
            )?;
            self.publish_events(event_sender, &approval_events).await;
            emitted.extend(approval_events);
            receipt
        } else {
            let (receipt, completion_events) = self.store.complete_prompt_atomic(
                &request.actor,
                &request.idempotency_key,
                &run_id,
            )?;
            self.publish_events(event_sender, &completion_events).await;
            emitted.extend(completion_events);
            receipt
        };
        Ok(CoreOutcome { receipt, emitted })
    }

    pub async fn resolve_approval(
        &self,
        actor: &str,
        approval_id: &str,
        expected_revision: i64,
        approved: bool,
        idempotency_key: &str,
        event_sender: &mpsc::Sender<Event>,
    ) -> SpikeResult<CoreOutcome> {
        let request_hash = hash_json(&json!({
            "approvalId": approval_id,
            "approved": approved,
            "expectedRevision": expected_revision
        }))?;
        let resolution = self.store.resolve_approval_atomic(
            actor,
            approval_id,
            expected_revision,
            approved,
            idempotency_key,
            &request_hash,
            self.policy.approval_failpoint,
        )?;
        if !resolution.replayed {
            self.publish_events(event_sender, &resolution.events).await;
        }
        Ok(CoreOutcome {
            receipt: resolution.receipt,
            emitted: resolution.events,
        })
    }

    async fn fail_worker(
        &self,
        request: &PromptRequest,
        run_id: &str,
        code: &str,
        emitted: &mut Vec<Event>,
        event_sender: &mpsc::Sender<Event>,
    ) -> SpikeResult<CoreOutcome> {
        let (receipt, failure_events) = self.store.fail_prompt_atomic(
            &request.actor,
            &request.idempotency_key,
            run_id,
            "worker.failed",
            "supervisor",
            code,
            None,
        )?;
        self.publish_events(event_sender, &failure_events).await;
        emitted.extend(failure_events);
        Ok(CoreOutcome {
            receipt,
            emitted: emitted.to_vec(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn fail_tool(
        &self,
        request: &PromptRequest,
        run_id: &str,
        call_id: &str,
        code: &str,
        emitted: &mut Vec<Event>,
        event_sender: &mpsc::Sender<Event>,
    ) -> SpikeResult<CoreOutcome> {
        let (receipt, failure_events) = self.store.fail_prompt_atomic(
            &request.actor,
            &request.idempotency_key,
            run_id,
            "tool.failed",
            "security",
            code,
            Some(call_id),
        )?;
        self.publish_events(event_sender, &failure_events).await;
        emitted.extend(failure_events);
        Ok(CoreOutcome {
            receipt,
            emitted: emitted.to_vec(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn emit(
        &self,
        emitted: &mut Vec<Event>,
        event_sender: &mpsc::Sender<Event>,
        run_id: &str,
        kind: &str,
        payload: Value,
        actor: &str,
        source: &str,
        causation_id: Option<&str>,
    ) -> SpikeResult<()> {
        let event =
            self.store
                .append_event(run_id, kind, &payload, actor, source, None, causation_id)?;
        let _ = event_sender.send(event.clone()).await;
        emitted.push(event);
        Ok(())
    }

    async fn publish_events(&self, event_sender: &mpsc::Sender<Event>, events: &[Event]) {
        for event in events {
            if event_sender.send(event.clone()).await.is_err() {
                break;
            }
        }
    }
}
