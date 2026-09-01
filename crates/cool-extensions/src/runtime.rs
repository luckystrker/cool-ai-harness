use std::collections::HashMap;
use std::sync::Arc;

use cool_protocol::CanonicalEvent;
use cool_security::CapabilityPolicy;
use futures_util::future::join_all;
use tokio::sync::Mutex;

use crate::{
    CompatibilityAdapter, CompatibilityWorkerSupervisor, HookDeclaration, HookEngine, HookError,
    HookInvocation, HookReviewStore, McpClient, PluginStore, WorkerError, WorkerLaunchSpec,
    plugin_status_event,
};

#[derive(Clone)]
pub struct ExtensionRuntime {
    hooks: Arc<Vec<(String, HookDeclaration)>>,
    engine: HookEngine,
    status: Arc<Mutex<Vec<CanonicalEvent>>>,
    workers: CompatibilityWorkerSupervisor,
}

impl ExtensionRuntime {
    pub fn from_store(store: &PluginStore) -> Result<Self, crate::StoreError> {
        let reviews = HookReviewStore::default();
        let mut hooks = Vec::new();
        let mut mcp = HashMap::new();
        let mut status = Vec::new();
        for loaded in store.load_enabled_isolated()? {
            match loaded {
                Ok(bundle) => {
                    let Some(manifest) = bundle.manifest else {
                        continue;
                    };
                    for (hook, hash) in store.reviewed_hook_hashes(&manifest.name)? {
                        reviews
                            .approve(&manifest.name, &hook, &hash)
                            .map_err(|error| crate::StoreError::Invalid(error.to_string()))?;
                    }
                    for server in bundle.mcp_servers {
                        mcp.insert(
                            format!("{}/{}", manifest.name, server.name()),
                            McpClient::new(server),
                        );
                    }
                    hooks.extend(
                        bundle
                            .hooks
                            .into_iter()
                            .map(|hook| (manifest.name.clone(), hook)),
                    );
                    status.push(plugin_status_event(manifest.name, "enabled", None));
                }
                Err(error) => status.push(plugin_status_event(
                    "unknown",
                    "failed",
                    Some(error.to_string()),
                )),
            }
        }
        hooks.sort_by(|left, right| {
            left.1
                .order
                .cmp(&right.1.order)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        Ok(Self {
            hooks: Arc::new(hooks),
            engine: HookEngine::new(reviews, mcp, store.root().join("hook-audit.jsonl")),
            status: Arc::new(Mutex::new(status)),
            workers: CompatibilityWorkerSupervisor::default(),
        })
    }

    pub async fn start_worker(
        &self,
        adapter: CompatibilityAdapter,
        spec: WorkerLaunchSpec,
    ) -> Result<(), WorkerError> {
        match self.workers.start(adapter, spec).await {
            Ok(event) => {
                self.status.lock().await.push(event);
                Ok(())
            }
            Err(error) => {
                self.status.lock().await.push(CanonicalEvent::WorkerFailed(
                    cool_protocol::WorkerEvent {
                        worker_id: format!("{}-compatibility", adapter.protocol_name()),
                        attempt: 1,
                        code: Some(error.to_string()),
                    },
                ));
                Err(error)
            }
        }
    }

    pub async fn report_plugin_status(
        &self,
        plugin: impl Into<String>,
        status: impl Into<String>,
        code: Option<String>,
    ) {
        self.status
            .lock()
            .await
            .push(plugin_status_event(plugin, status, code));
    }

    pub async fn lifecycle_event(
        &self,
        event: &str,
        payload: serde_json::Value,
        policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent> {
        let mut events = if event == "SessionStart" {
            let mut status = self.status.lock().await.clone();
            status.extend(self.workers.heartbeat().await);
            status
        } else {
            Vec::new()
        };
        let invocation = HookInvocation {
            event: event.to_owned(),
            fields: payload
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            payload,
        };
        let matching = self
            .hooks
            .iter()
            .filter(|(_, hook)| hook.event == event)
            .cloned()
            .collect::<Vec<_>>();
        for group in matching.chunk_by(|left, right| left.1.order == right.1.order) {
            let (parallel, serial): (Vec<_>, Vec<_>) =
                group.iter().cloned().partition(|(_, hook)| hook.parallel);
            let parallel_results = join_all(parallel.into_iter().map(|(plugin, hook)| {
                let invocation = invocation.clone();
                async move {
                    (
                        plugin.clone(),
                        self.engine
                            .invoke(&plugin, &hook, &invocation, policy, policy)
                            .await,
                    )
                }
            }))
            .await;
            for (plugin, result) in parallel_results {
                collect_hook_result(&mut events, plugin, result);
            }
            for (plugin, hook) in serial {
                let result = self
                    .engine
                    .invoke(&plugin, &hook, &invocation, policy, policy)
                    .await;
                collect_hook_result(&mut events, plugin, result);
            }
        }
        events
    }
}

fn collect_hook_result(
    events: &mut Vec<CanonicalEvent>,
    plugin: String,
    result: Result<serde_json::Value, HookError>,
) {
    match result {
        Ok(_) | Err(HookError::Unreviewed | HookError::ApprovalRequired | HookError::Denied) => {}
        Err(error) => events.push(plugin_status_event(
            plugin,
            "degraded",
            Some(error.to_string()),
        )),
    }
}
