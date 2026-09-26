//! Deep Research executor (Python `app/research/orchestrator.py` parity).
//!
//! One `ResearchRun` drives the pipeline: **decompose** (one model call
//! splitting the topic into `depth` sub-questions) → **gather** (one
//! `researcher` subagent per sub-question, bounded concurrency) → **collect**
//! (sources parsed out of each subagent's findings) → **synthesize** (one
//! model call writing a cited markdown report) → **persist** (report stored as
//! a content-addressed artifact; the run row is finalized with
//! sources/citations/usage).
//!
//! Cancellation flows through a `watch` channel shared with the canonical run
//! record: a `run.cancel` (or `research.cancel`) signal stops the pipeline and
//! every in-flight researcher subagent. Mid-run progress (`sub_questions`,
//! `sources`) is written to the row between stages so a poll of
//! `research.get` sees live state even though progress events are transient.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use cool_agent::{AgentRuntime, EventSink, Message, MessageRole, ModelEvent, ModelRequest, Usage};
use cool_protocol::{
    CanonicalEvent, ResearchSource, ResearchStage, ResearchStarted, ResearchSubquestion,
    ResearchTerminal, RunStarted, RunTerminal,
};
use cool_security::mask_secrets;
use cool_store::domains::artifacts::NewArtifact;
use cool_store::domains::conversations::NewConversation;
use cool_store::domains::research::{NewResearchRun, ResearchRun, TERMINAL_RESEARCH_STATUSES};
use cool_store::time::now_python;
use cool_store::{LegacyStore, StoreError};
use futures_util::StreamExt;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use crate::AppServerEventSink;
use crate::subagents::{SubagentExecutor, SubagentLaunchSpec};

/// Python `RESEARCH_MAX_CONCURRENT_SUBAGENTS`.
const MAX_CONCURRENT_SUBAGENTS: usize = 3;
/// Python `MAX_SOURCES_PER_SUBAGENT` / `MAX_TOTAL_SOURCES` / `MAX_SNIPPET_CHARS`.
const MAX_SOURCES_PER_SUBAGENT: usize = 8;
const MAX_TOTAL_SOURCES: usize = 40;
const MAX_SNIPPET_CHARS: usize = 400;
const MAX_CITATION_TEXT_CHARS: usize = 400;
fn url_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s)\]}>"']+"#).expect("url regex"))
}

fn numbered_line_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\d+[.)]\s+(.+)$").expect("numbered line regex"))
}

fn sentence_boundary_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[.!?]\s|\n").expect("sentence boundary regex"))
}

fn citation_marker_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[(\d{1,3})\]").expect("citation marker regex"))
}

const CITE_JSON_MARKER: &str = "CITE_JSON:";

/// Shared cancel channels per `research_runs.id`; the same sender is installed
/// in the canonical run record, so `run.cancel` and `research.cancel` land on
/// one channel.
type LiveMap = HashMap<i64, watch::Sender<Option<String>>>;

fn lock_live(live: &StdMutex<LiveMap>) -> std::sync::MutexGuard<'_, LiveMap> {
    live.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// What a finished pipeline hands back to callers that need the artifacts
/// (the `deep_research` tool returns the report inline).
pub struct ResearchOutcome {
    pub run_id: i64,
    pub status: &'static str,
    pub report: Option<String>,
    pub report_artifact_id: Option<i64>,
    pub source_count: u32,
    pub error: Option<String>,
}

/// Runs the deep-research pipeline against the legacy store.
pub struct ResearchExecutor {
    store: Arc<LegacyStore>,
    subagents: Arc<SubagentExecutor>,
    driver: Arc<dyn cool_agent::ModelDriver>,
    default_model: String,
    /// Root of the content-addressed blob store (`data_dir/artifacts`); when
    /// absent (unit tests with no filesystem layout) the report is still
    /// persisted on the row without an artifact.
    artifacts_dir: Option<PathBuf>,
    live: StdMutex<LiveMap>,
}

impl ResearchExecutor {
    pub fn new(
        store: Arc<LegacyStore>,
        subagents: Arc<SubagentExecutor>,
        runtime: &AgentRuntime,
        default_model: String,
        artifacts_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            subagents,
            driver: runtime.driver(),
            default_model,
            artifacts_dir,
            live: StdMutex::new(HashMap::new()),
        }
    }

    /// Register the cancel channel for a research run about to execute. The
    /// caller installs the same sender in the canonical `runs` map.
    pub fn register(&self, research_run_id: i64, sender: watch::Sender<Option<String>>) {
        lock_live(&self.live).insert(research_run_id, sender);
    }

    pub fn unregister(&self, research_run_id: i64) {
        lock_live(&self.live).remove(&research_run_id);
    }

    /// Signal a live run to stop (the `research.cancel` dispatch path — the row
    /// is already flipped to `cancelled` by the store call).
    pub fn signal_cancel(&self, research_run_id: i64) {
        if let Some(sender) = lock_live(&self.live).get(&research_run_id) {
            let _ = sender.send(Some("cancelled".to_owned()));
        }
    }

    /// Canonical-run wrapper: emits `RunStarted`/`ResearchStarted`, drives the
    /// pipeline, then the matching `research.*` + run terminal events. Called
    /// from `AppServer::run_research` in a spawned task.
    pub(crate) async fn execute(
        &self,
        run_id: i64,
        sink: &AppServerEventSink,
        mut cancel_rx: watch::Receiver<Option<String>>,
    ) {
        let actor = crate::local_actor();
        let _guard = ResearchLiveGuard {
            executor: self,
            research_run_id: run_id,
        };
        let _ = sink
            .emit(CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: Some("research".to_owned()),
            }))
            .await;
        let _ = sink
            .emit(CanonicalEvent::ResearchStarted(ResearchStarted {
                research_run_id: run_id.to_string(),
            }))
            .await;
        let run = match self.store.get_research_run(&actor.id, run_id) {
            Ok(run) => run,
            Err(_) => {
                let _ = sink
                    .emit(CanonicalEvent::RunFailed(RunTerminal {
                        reason: "research_run_not_found".to_owned(),
                        error_code: Some("not_found".to_owned()),
                    }))
                    .await;
                return;
            }
        };
        let outcome = self.pipeline(&run, Some(sink), &mut cancel_rx).await;
        self.finish_canonical(&outcome, sink).await;
    }

    /// Inline driver for the `deep_research` tool: creates the run row (with a
    /// hidden research conversation when `conversation_id` is absent) and runs
    /// the pipeline to completion with no canonical sink. `cancel` is the
    /// calling run's cancel signal when present.
    pub async fn run_inline(
        self: &Arc<Self>,
        topic: &str,
        depth: i64,
        model: Option<String>,
        conversation_id: Option<i64>,
        cancel: Option<watch::Receiver<Option<String>>>,
    ) -> Result<ResearchOutcome, StoreError> {
        let actor = crate::local_actor();
        let conversation_id = match conversation_id {
            Some(id) => id,
            None => create_research_conversation(&self.store, &actor.id, topic, model.as_deref())?,
        };
        let run = self.store.create_research_run(
            &actor.id,
            &NewResearchRun {
                topic: topic.to_owned(),
                depth,
                model,
                conversation_id: Some(conversation_id),
                parent_task_run_id: None,
            },
        )?;
        let (cancel_tx, default_rx) = watch::channel(None);
        let mut cancel_rx = cancel.unwrap_or(default_rx);
        self.register(run.id, cancel_tx);
        let outcome = self.pipeline(&run, None, &mut cancel_rx).await;
        self.unregister(run.id);
        Ok(outcome)
    }

    /// The five pipeline stages. `sink` carries transient progress events when
    /// a canonical run is streaming them to a client.
    async fn pipeline(
        &self,
        run: &ResearchRun,
        sink: Option<&AppServerEventSink>,
        cancel_rx: &mut watch::Receiver<Option<String>>,
    ) -> ResearchOutcome {
        let actor = crate::local_actor();
        let model = run
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        if model.is_empty() {
            return self
                .fail(
                    &actor.id,
                    run,
                    sink,
                    "No model configured for research".to_owned(),
                )
                .await;
        }
        let mut usage = Usage::default();

        // --- Stage 1: decompose -------------------------------------------
        emit_stage(sink, "decompose").await;
        let (sub_questions, decompose_usage) =
            match self.decompose(&model, &run.topic, run.depth).await {
                Ok(pair) => pair,
                Err(message) => {
                    return self.fail(&actor.id, run, sink, message).await;
                }
            };
        accumulate(&mut usage, &decompose_usage);
        if sub_questions.is_empty() {
            return self
                .fail(
                    &actor.id,
                    run,
                    sink,
                    "Topic decomposition returned no sub-questions".to_owned(),
                )
                .await;
        }
        let _ = self.store.update_research_progress(
            &actor.id,
            run.id,
            Some(&json!(sub_questions)),
            None,
        );
        if self.is_cancelled(&actor.id, run.id, cancel_rx) {
            return self.cancel(&actor.id, run, sink).await;
        }

        // --- Stage 2: gather ----------------------------------------------
        emit_stage(sink, "gather").await;
        let findings = self
            .gather(&actor.id, run, &model, &sub_questions, sink, cancel_rx)
            .await;
        let findings = match findings {
            GatherOutcome::Done(findings) => findings,
            GatherOutcome::Cancelled => return self.cancel(&actor.id, run, sink).await,
        };

        // --- Stage 3: collect sources --------------------------------------
        let mut sources: Vec<Value> = Vec::new();
        for (question, text) in sub_questions.iter().zip(findings.iter()) {
            if text.is_empty() {
                continue;
            }
            sources.extend(extract_sources(text, question));
        }
        let sources = dedupe_sources(sources);
        let _ = self.store.update_research_progress(
            &actor.id,
            run.id,
            None,
            Some(&Value::Array(sources.clone())),
        );
        for source in &sources {
            if let Some(sink) = sink {
                let _ = sink
                    .emit(CanonicalEvent::ResearchSourceFound(ResearchSource {
                        url: source
                            .get("url")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        title: source
                            .get("title")
                            .and_then(Value::as_str)
                            .filter(|title| !title.is_empty())
                            .map(str::to_owned),
                        snippet: source
                            .get("snippet")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        confidence: source.get("confidence").and_then(Value::as_f64),
                    }))
                    .await;
            }
        }
        if self.is_cancelled(&actor.id, run.id, cancel_rx) {
            return self.cancel(&actor.id, run, sink).await;
        }

        // --- Stage 4: synthesize -------------------------------------------
        emit_stage(sink, "synthesize").await;
        let (report, citations, synth_usage) = match self
            .synthesize(&model, &run.topic, &sub_questions, &sources)
            .await
        {
            Ok(result) => result,
            Err(message) => {
                return self.fail(&actor.id, run, sink, message).await;
            }
        };
        accumulate(&mut usage, &synth_usage);
        let report = report.trim().to_owned();
        if report.is_empty() {
            return self
                .fail(
                    &actor.id,
                    run,
                    sink,
                    "Synthesis returned an empty report".to_owned(),
                )
                .await;
        }
        if self.is_cancelled(&actor.id, run.id, cancel_rx) {
            return self.cancel(&actor.id, run, sink).await;
        }

        // --- Stage 5: persist -----------------------------------------------
        let artifact_id = self.persist_report(&actor.id, run, &report, &model);
        let usage_json = usage_json(&usage);
        // A concurrent `research.cancel` wins over this completion.
        let current = self.store.get_research_run(&actor.id, run.id).ok();
        if current
            .as_ref()
            .is_some_and(|stored| stored.status == "cancelled")
        {
            return self.cancel(&actor.id, run, sink).await;
        }
        match self.store.finish_research_run(
            &actor.id,
            run.id,
            "completed",
            Some(&report),
            Some(&Value::Array(sources.clone())),
            Some(&Value::Array(citations)),
            Some(&json!(sub_questions)),
            Some(&usage_json),
            None,
            artifact_id,
        ) {
            Ok(_) => ResearchOutcome {
                run_id: run.id,
                status: "completed",
                report: Some(report),
                report_artifact_id: artifact_id,
                source_count: sources.len() as u32,
                error: None,
            },
            Err(error) => {
                return self
                    .fail(&actor.id, run, sink, mask_secrets(&error.to_string()))
                    .await;
            }
        }
    }

    /// One-shot completion through the provider driver (collects the stream).
    async fn complete(
        &self,
        model: &str,
        prompt: String,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<(String, Usage), String> {
        let request = ModelRequest {
            model: model.to_owned(),
            messages: vec![Message::text(MessageRole::User, prompt)],
            tools: Vec::new(),
            temperature,
            max_tokens: Some(max_tokens),
        };
        let mut stream = self
            .driver
            .stream(request)
            .await
            .map_err(|error| mask_secrets(&error.to_string()))?;
        let mut content = String::new();
        let mut usage = Usage::default();
        while let Some(event) = stream.next().await {
            match event {
                Ok(ModelEvent::Content(text)) => content.push_str(&text),
                Ok(ModelEvent::Usage(u)) => usage = u,
                Ok(ModelEvent::Finish { .. }) => break,
                Ok(_) => {}
                Err(error) => return Err(mask_secrets(&error.to_string())),
            }
        }
        Ok((content, usage))
    }

    /// Stage 1: split the topic into `depth` numbered sub-questions.
    async fn decompose(
        &self,
        model: &str,
        topic: &str,
        depth: i64,
    ) -> Result<(Vec<String>, Usage), String> {
        let prompt = format!(
            "Decompose the following research topic into exactly {depth} distinct, \
             focused research questions.\n\nTopic: {topic}\n\nEach question must \
             target a separate aspect of the topic so the answers together cover \
             it fully. Return ONLY a numbered list, one question per line, in the \
             format: 1. question text"
        );
        let (content, usage) = self.complete(model, prompt, 0.4, 1000).await?;
        let mut questions = Vec::new();
        for line in content.lines() {
            if let Some(capture) = numbered_line_regex().captures(line)
                && let Some(question) = capture
                    .get(1)
                    .map(|match_| match_.as_str().trim().to_owned())
                && !question.is_empty()
            {
                questions.push(question);
            }
        }
        questions.truncate(depth as usize);
        Ok((questions, usage))
    }

    /// Stage 2: one researcher subagent per sub-question, bounded concurrency.
    async fn gather(
        &self,
        actor_id: &str,
        run: &ResearchRun,
        model: &str,
        sub_questions: &[String],
        sink: Option<&AppServerEventSink>,
        cancel_rx: &mut watch::Receiver<Option<String>>,
    ) -> GatherOutcome {
        let researcher_role = self
            .store
            .list_subagent_roles()
            .ok()
            .and_then(|roles| roles.into_iter().find(|role| role.name == "researcher"));
        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_SUBAGENTS));
        let mut set: JoinSet<(usize, Result<String, String>)> = JoinSet::new();
        let total = sub_questions.len();
        for (index, question) in sub_questions.iter().cloned().enumerate() {
            let permit = Arc::clone(&semaphore);
            let executor = self.subagents.clone();
            let actor = actor_id.to_owned();
            let topic = run.topic.clone();
            let model = model.to_owned();
            let role_id = researcher_role.as_ref().map(|role| role.id);
            let research_run_id = run.id;
            // Python passes `conversation_id or 1`; the Rust dispatch always
            // creates the hidden conversation, so 0 is unreachable in
            // practice but kept as a defensive fallback.
            let conversation_id = run.conversation_id.unwrap_or(0);
            let progress = sink.cloned();
            let mut child_cancel = cancel_rx.clone();
            set.spawn(async move {
                let _permit = permit.acquire().await.expect("semaphore open");
                if let Some(sink) = &progress {
                    let _ = sink
                        .emit(CanonicalEvent::ResearchSubquestionStarted(
                            ResearchSubquestion {
                                index: index as u32,
                                question: question.clone(),
                                status: "running".to_owned(),
                            },
                        ))
                        .await;
                }
                let prompt = researcher_prompt(&topic, &question, index, total);
                let key = format!("research-gather:{research_run_id}:{index}");
                let spec = SubagentLaunchSpec {
                    parent_conversation_id: conversation_id,
                    role_id,
                    profile_id: None,
                    parent_run_id: None,
                    research_run_id: Some(research_run_id),
                    name: Some(format!("research-sq-{}", index + 1)),
                    prompt,
                    model: Some(model.clone()),
                };
                let child = match executor.launch(&actor, spec, &key, &key).await {
                    Ok(child) => child,
                    Err(error) => return (index, Err(mask_secrets(&error.to_string()))),
                };
                let row = match executor
                    .await_terminal(&actor, child.id, &mut child_cancel)
                    .await
                {
                    Ok(row) => row,
                    Err(error) => return (index, Err(mask_secrets(&error.to_string()))),
                };
                // The research run's own cancel fires the shared receiver and
                // signals the child inside `await_terminal`.
                if child_cancel.borrow().is_some() {
                    return (index, Ok(String::new()));
                }
                if row.status == "completed" {
                    (index, Ok(row.result_summary.unwrap_or_default()))
                } else {
                    (index, Err(row.error.unwrap_or_else(|| row.status.clone())))
                }
            });
        }
        let mut findings = vec![String::new(); sub_questions.len()];
        let mut cancelled = false;
        while let Some(result) = set.join_next().await {
            if cancel_rx.borrow().is_some() {
                cancelled = true;
            }
            match result {
                Ok((index, outcome)) => {
                    let status = match &outcome {
                        Ok(text) if !text.is_empty() => "completed",
                        Ok(_) => "empty",
                        Err(_) => "failed",
                    };
                    if let Some(sink) = sink {
                        let _ = sink
                            .emit(CanonicalEvent::ResearchSubquestionCompleted(
                                ResearchSubquestion {
                                    index: index as u32,
                                    question: sub_questions.get(index).cloned().unwrap_or_default(),
                                    status: status.to_owned(),
                                },
                            ))
                            .await;
                    }
                    findings[index] = outcome.unwrap_or_default();
                }
                Err(_join_error) => {}
            }
        }
        if cancelled {
            return GatherOutcome::Cancelled;
        }
        GatherOutcome::Done(findings)
    }

    /// Stage 4: write the cited markdown report.
    async fn synthesize(
        &self,
        model: &str,
        topic: &str,
        sub_questions: &[String],
        sources: &[Value],
    ) -> Result<(String, Vec<Value>, Usage), String> {
        let source_lines: Vec<String> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                format!(
                    "[{}] {} — {}",
                    index + 1,
                    source
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    source
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|title| !title.is_empty())
                        .unwrap_or("(no title)")
                )
            })
            .collect();
        let question_lines: Vec<String> = sub_questions
            .iter()
            .map(|question| format!("- {question}"))
            .collect();
        let prompt = format!(
            "You are a research analyst. Below are findings gathered from {} web \
             sources about the topic: {topic}\n\nSOURCES:\n{}\n\nSub-questions to \
             answer in the analysis:\n{}\n\nWrite a structured markdown report:\n\
             # {topic}\n## Executive Summary\n(2-3 sentences)\n## Key Findings\n\
             (numbered, most important first)\n## Detailed Analysis\n(one \
             subsection per sub-question, with evidence)\n## Limitations & Gaps\n\
             (what could not be verified)\n## Sources\n(the numbered URL list, \
             always the final section)\n\nCITING: every factual claim must be \
             followed by its source number(s) like [1] or [2][3]. Every source in \
             ## Sources must be cited at least once. If sources disagree, say so \
             explicitly and label it a conflict.\nAfter the report, output a \
             single line starting with CITE_JSON: followed by a JSON object: \
             {{\"citations\": [{{\"index\": 1, \"confidence\": \"high|medium|low\", \
             \"conflict\": false}}]}} — one entry per citation index used in the report.",
            sources.len(),
            source_lines.join("\n"),
            question_lines.join("\n"),
        );
        let (content, usage) = self.complete(model, prompt, 0.3, 4000).await?;
        let citations = extract_citations(&content, sources.len());
        Ok((content, citations, usage))
    }

    /// Stage 5: content-addressed blob write + `artifacts` row.
    fn persist_report(
        &self,
        actor_id: &str,
        run: &ResearchRun,
        report: &str,
        model: &str,
    ) -> Option<i64> {
        let (dir, conversation_id) = match (self.artifacts_dir.clone(), run.conversation_id) {
            (Some(dir), Some(conversation_id)) => (dir, conversation_id),
            _ => return None,
        };
        let digest = Sha256::digest(report.as_bytes());
        let sha = format!("{digest:x}");
        let relative = format!("{}/{}", &sha[..2], sha);
        let path = dir.join(&relative);
        if !path.exists() {
            if std::fs::create_dir_all(path.parent()?).is_err() {
                return None;
            }
            if std::fs::write(&path, report.as_bytes()).is_err() {
                return None;
            }
        }
        self.store
            .register_artifact(
                actor_id,
                conversation_id,
                &NewArtifact {
                    filename: format!("research-{}.md", run.id),
                    media_type: "text/markdown".to_owned(),
                    kind: "report".to_owned(),
                    size_bytes: report.len() as i64,
                    sha256: Some(sha),
                    storage_path: relative,
                    tool_call_id: None,
                    parent_id: None,
                    metadata: Some(json!({
                        "research_run_id": run.id,
                        "topic": run.topic,
                        "model": model,
                    })),
                    run_id: None,
                },
            )
            .map(|artifact| artifact.id)
            .ok()
    }

    /// True once the shared cancel channel fired or the row went `cancelled`
    /// (a `research.cancel` that raced the executor signals through the row).
    fn is_cancelled(
        &self,
        actor_id: &str,
        run_id: i64,
        cancel_rx: &watch::Receiver<Option<String>>,
    ) -> bool {
        if cancel_rx.borrow().is_some() {
            return true;
        }
        if let Ok(row) = self.store.get_research_run(actor_id, run_id)
            && row.status == "cancelled"
        {
            return true;
        }
        false
    }

    async fn finish_canonical(&self, outcome: &ResearchOutcome, sink: &AppServerEventSink) {
        let terminal = ResearchTerminal {
            artifact_id: outcome.report_artifact_id.map(|id| id.to_string()),
            source_count: outcome.source_count,
            error: outcome.error.clone(),
        };
        match outcome.status {
            "completed" => {
                let _ = sink.emit(CanonicalEvent::ResearchCompleted(terminal)).await;
                let _ = sink
                    .emit(CanonicalEvent::RunCompleted(RunTerminal {
                        reason: "research_completed".to_owned(),
                        error_code: None,
                    }))
                    .await;
            }
            "cancelled" => {
                let _ = sink.emit(CanonicalEvent::ResearchCancelled(terminal)).await;
                let _ = sink
                    .emit(CanonicalEvent::RunCancelled(RunTerminal {
                        reason: "research_cancelled".to_owned(),
                        error_code: None,
                    }))
                    .await;
            }
            _ => {
                let _ = sink.emit(CanonicalEvent::ResearchFailed(terminal)).await;
                let _ = sink
                    .emit(CanonicalEvent::RunFailed(RunTerminal {
                        reason: "research_failed".to_owned(),
                        error_code: outcome.error.clone().map(|_| "research_failed".to_owned()),
                    }))
                    .await;
            }
        }
    }

    async fn fail(
        &self,
        actor_id: &str,
        run: &ResearchRun,
        _sink: Option<&AppServerEventSink>,
        error: String,
    ) -> ResearchOutcome {
        let error = mask_secrets(&error);
        let _ = self.store.finish_research_run(
            actor_id,
            run.id,
            "failed",
            None,
            None,
            None,
            None,
            None,
            Some(&error),
            None,
        );
        ResearchOutcome {
            run_id: run.id,
            status: "failed",
            report: None,
            report_artifact_id: None,
            source_count: 0,
            error: Some(error),
        }
    }

    async fn cancel(
        &self,
        actor_id: &str,
        run: &ResearchRun,
        _sink: Option<&AppServerEventSink>,
    ) -> ResearchOutcome {
        // The dispatch path flips the row already; `cancel_research_run` is a
        // no-op on a terminal row, so call it unconditionally.
        let _ = self.store.cancel_research_run(actor_id, run.id);
        ResearchOutcome {
            run_id: run.id,
            status: "cancelled",
            report: None,
            report_artifact_id: None,
            source_count: 0,
            error: None,
        }
    }
}

enum GatherOutcome {
    Done(Vec<String>),
    Cancelled,
}

/// Hidden `[Research] {topic}` conversation hosting the researcher subagents
/// and the report artifact (Python `start_research`, `conversation_id=None`).
/// Returns the new conversation id.
pub(crate) fn create_research_conversation(
    store: &LegacyStore,
    actor_id: &str,
    topic: &str,
    model: Option<&str>,
) -> Result<i64, StoreError> {
    let title = format!("[Research] {}", topic.chars().take(60).collect::<String>());
    let conversation = store.create_conversation(
        actor_id,
        &NewConversation {
            title: Some(title),
            model: model.map(str::to_owned),
            metadata: Some(json!({"is_research": true})),
            ..NewConversation::default()
        },
    )?;
    Ok(conversation.id)
}

/// Removes the live entry when the execute task ends (including panics).
struct ResearchLiveGuard<'a> {
    executor: &'a ResearchExecutor,
    research_run_id: i64,
}

impl Drop for ResearchLiveGuard<'_> {
    fn drop(&mut self) {
        self.executor.unregister(self.research_run_id);
        // A panicked pipeline must not leave the row `running` forever.
        let actor = crate::local_actor();
        if let Ok(run) = self
            .executor
            .store
            .get_research_run(&actor.id, self.research_run_id)
            && !TERMINAL_RESEARCH_STATUSES.contains(&run.status.as_str())
        {
            let _ = self.executor.store.finish_research_run(
                &actor.id,
                run.id,
                "failed",
                None,
                None,
                None,
                None,
                None,
                Some("executor terminated abnormally"),
                None,
            );
        }
    }
}

async fn emit_stage(sink: Option<&AppServerEventSink>, stage: &str) {
    if let Some(sink) = sink {
        let _ = sink
            .emit(CanonicalEvent::ResearchStage(ResearchStage {
                stage: stage.to_owned(),
                message: None,
                progress: None,
            }))
            .await;
    }
}

/// The researcher-subagent prompt (Python `_run_researcher_subagent`).
fn researcher_prompt(topic: &str, question: &str, index: usize, total: usize) -> String {
    format!(
        "Research question: {question}\n\nContext: this is sub-question {} of \
         {total} for the topic:\n{topic}\n\nUse web_search to find relevant \
         sources and web_fetch to read the most promising ones. For dynamic or \
         script-rendered pages, use browser_navigate and browser_extract; \
         capture a browser_screenshot and analyze it when a chart or diagram \
         carries evidence. Then write your findings:\n- Present 3-8 distinct \
         findings as numbered claims.\n- Immediately after each claim, put the \
         supporting URL(s) in [brackets].\n- End with a '## Sources' section \
         listing every URL you used, one per line, in the format: URL | Title \
         of the page\nKeep the whole response under 1500 words. Report facts \
         only; note explicitly when information is uncertain.",
        index + 1,
    )
}

/// Parse a subagent's findings into structured sources (Python
/// `_extract_sources` + `_lookup_title` + `_snippet_around`).
fn extract_sources(findings: &str, sub_question: &str) -> Vec<Value> {
    let now = now_python();
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for matched in url_regex().find_iter(findings) {
        let url = matched
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', ')', ']'])
            .to_owned();
        if url.is_empty() || !seen.insert(url.clone()) {
            continue;
        }
        let title = lookup_title(findings, &url);
        let snippet = snippet_around(findings, matched.start());
        sources.push(json!({
            "url": url,
            "title": title,
            "snippet": snippet,
            "fetched_at": now,
            "sub_question": sub_question,
            "confidence": if title.is_empty() { "medium" } else { "high" },
            "conflict": false,
        }));
        if sources.len() >= MAX_SOURCES_PER_SUBAGENT {
            break;
        }
    }
    sources
}

/// Find `URL | Title` (or `URL - Title`) in the Sources section.
fn lookup_title(findings: &str, url: &str) -> String {
    let mut section = "";
    for marker in ["## Sources", "## Sources:", "# Sources", "Sources:"] {
        if let Some(index) = findings.find(marker) {
            section = &findings[index..];
            break;
        }
    }
    for line in section.lines() {
        if !line.contains(url) {
            continue;
        }
        if let Some((_, title)) = line.split_once('|') {
            let title = title.trim();
            if !title.is_empty() && !title.contains('|') {
                return title.chars().take(200).collect();
            }
        }
        for separator in [" — ", " - ", " | "] {
            if let Some(title) = line.split_once(separator).map(|(_, rest)| rest.trim())
                && !title.is_empty()
            {
                return title.chars().take(200).collect();
            }
        }
        // Markdown link [Title](url)
        if let Some(start) = line.find('[')
            && let Some(mid) = line.find("](")
            && mid > start
            && let Some(end) = line[mid + 2..].find(')')
        {
            let href = &line[mid + 2..mid + 2 + end];
            if href.contains(url) || url.contains(href) {
                return line[start + 1..mid].trim().chars().take(200).collect();
            }
        }
        return String::new();
    }
    String::new()
}

/// A readable snippet around `position`, trimmed to a fixed window (Python
/// `_snippet_around`: 140 chars before, 260 after, ellipsized).
fn snippet_around(text: &str, position: usize) -> String {
    let start = position.saturating_sub(140);
    let end = (position + 260).min(text.len());
    let mut snippet = text[start..end].replace('\n', " ").trim().to_owned();
    if start > 0 {
        snippet = format!("…{snippet}");
    }
    if end < text.len() {
        snippet.push('…');
    }
    snippet.chars().take(MAX_SNIPPET_CHARS).collect()
}

/// One entry per URL; duplicates merge their `sub_question` tags.
fn dedupe_sources(sources: Vec<Value>) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut by_url: HashMap<String, Value> = HashMap::new();
    for source in sources {
        let Some(url) = source.get("url").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        match by_url.get_mut(&url) {
            None => {
                order.push(url.clone());
                by_url.insert(url, source);
            }
            Some(existing) => {
                let title = source
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !title.is_empty()
                    && existing
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .is_empty()
                {
                    existing["title"] = Value::String(title.to_owned());
                }
                let mut questions: Vec<String> = existing
                    .get("sub_questions")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        existing
                            .get("sub_question")
                            .and_then(Value::as_str)
                            .map(|q| vec![q.to_owned()])
                            .unwrap_or_default()
                    });
                if let Some(question) = source.get("sub_question").and_then(Value::as_str)
                    && !questions.iter().any(|known| known == question)
                {
                    questions.push(question.to_owned());
                }
                existing["sub_questions"] = json!(questions);
            }
        }
    }
    order
        .into_iter()
        .take(MAX_TOTAL_SOURCES)
        .filter_map(|url| by_url.get(&url).cloned())
        .collect()
}

/// Parse `[n]` markers and the trailing `CITE_JSON:` block into citation rows
/// (Python `_extract_citations`).
fn extract_citations(report: &str, source_count: usize) -> Vec<Value> {
    let mut annotations: HashMap<usize, (String, bool)> = HashMap::new();
    if let Some(start) = report.find(CITE_JSON_MARKER) {
        let raw = report[start + CITE_JSON_MARKER.len()..].trim();
        if let Ok(parsed) = serde_json::from_str::<Value>(raw)
            && let Some(entries) = parsed.get("citations").and_then(Value::as_array)
        {
            for entry in entries {
                let index = entry
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                annotations.insert(
                    index,
                    (
                        entry
                            .get("confidence")
                            .and_then(Value::as_str)
                            .unwrap_or("medium")
                            .to_owned(),
                        entry
                            .get("conflict")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ),
                );
            }
        }
    }
    let mut citations = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for matched in citation_marker_regex().find_iter(report) {
        let Some(index) = matched
            .as_str()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<usize>()
            .ok()
        else {
            continue;
        };
        if index == 0 || index > source_count || !seen.insert(index) {
            continue;
        }
        let claim = sentence_around(report, matched.start(), MAX_CITATION_TEXT_CHARS);
        let (confidence, conflict) = annotations
            .get(&index)
            .cloned()
            .unwrap_or_else(|| ("medium".to_owned(), false));
        citations.push(json!({
            "index": index,
            "text": claim,
            "source_ids": [index],
            "confidence": confidence,
            "conflict": conflict,
        }));
    }
    citations
}

/// The sentence containing `position` (Python `_sentence_around`: `.` only
/// terminates when followed by whitespace, so `2.0` doesn't split).
fn sentence_around(text: &str, position: usize, max_len: usize) -> String {
    let before = &text[..position.min(text.len())];
    let mut start = 0;
    for matched in sentence_boundary_regex().find_iter(before) {
        start = matched.end();
    }
    let after = &text[position.min(text.len())..];
    let end = position.min(text.len())
        + sentence_boundary_regex()
            .find(after)
            .map(|matched| matched.end())
            .unwrap_or(after.len());
    text[start..end].trim().chars().take(max_len).collect()
}

/// Fold one provider `Usage` into the Python `{prompt_tokens, …, cost_usd}`
/// accumulator shape.
fn accumulate(total: &mut Usage, usage: &Usage) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.total_tokens += usage.total_tokens;
    let micro = total.cost_micro_usd.unwrap_or(0) + usage.cost_micro_usd.unwrap_or(0);
    total.cost_micro_usd = Some(micro);
}

fn usage_json(usage: &Usage) -> Value {
    json!({
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
        "cost_usd": usage.cost_micro_usd.unwrap_or(0) as f64 / 1_000_000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_numbered_questions() {
        // decompose parsing is exercised through `numbered_line_regex`.
        let text = "1. First question\n2) Second question\n- not numbered\n3.   Third";
        let questions: Vec<String> = text
            .lines()
            .filter_map(|line| {
                numbered_line_regex()
                    .captures(line)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().trim().to_owned())
                    .filter(|q| !q.is_empty())
            })
            .collect();
        assert_eq!(questions, ["First question", "Second question", "Third"]);
    }

    #[test]
    fn extracts_sources_with_titles() {
        let findings = "Claim one [https://a.example/x]. More text.\n\n## Sources\n\
             https://a.example/x | Example A\nhttps://b.example/y — Example B\n";
        let sources = extract_sources(findings, "q1");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["url"], "https://a.example/x");
        assert_eq!(sources[0]["title"], "Example A");
        assert_eq!(sources[0]["confidence"], "high");
        assert_eq!(sources[1]["title"], "Example B");
    }

    #[test]
    fn dedupes_sources_merging_questions() {
        let sources = vec![
            json!({"url": "https://a", "title": "", "sub_question": "q1"}),
            json!({"url": "https://a", "title": "T", "sub_question": "q2"}),
            json!({"url": "https://b", "title": "B", "sub_question": "q3"}),
        ];
        let deduped = dedupe_sources(sources);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0]["title"], "T");
        assert_eq!(deduped[0]["sub_questions"], json!(["q1", "q2"]));
    }

    #[test]
    fn extracts_citations_with_annotations() {
        let report = "Sentence one claims a fact [1]. Another claim follows [2].\n\
             CITE_JSON: {\"citations\": [{\"index\": 1, \"confidence\": \"high\", \"conflict\": true}]}";
        let citations = extract_citations(report, 2);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0]["index"], 1);
        assert_eq!(citations[0]["confidence"], "high");
        assert_eq!(citations[0]["conflict"], true);
        assert_eq!(citations[1]["confidence"], "medium");
    }

    #[test]
    fn sentence_around_respects_decimals() {
        let text = "Version 2.0 shipped. The second sentence [1] is here.";
        let position = text.find("[1]").expect("marker");
        let claim = sentence_around(text, position, 400);
        assert_eq!(claim, "The second sentence [1] is here.");
    }
}
