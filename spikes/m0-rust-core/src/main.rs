use std::path::PathBuf;

use cool_m0_spike::core::CorePolicy;
use cool_m0_spike::store::{ApprovalFailpoint, PromptFailpoint};
use cool_m0_spike::worker::worker_stdio;
use cool_m0_spike::{SpikeCore, SpikeError, SpikeResult, Store, serve_jsonl};
use tokio::io::BufReader;

#[tokio::main]
async fn main() -> SpikeResult<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("worker") => worker_stdio().await,
        Some("app-server") => {
            let database = arguments.next().map(PathBuf::from).ok_or_else(|| {
                SpikeError::Protocol("usage: cool-m0-spike app-server <disposable-db>".to_owned())
            })?;
            let mut policy = CorePolicy::default();
            for argument in arguments {
                if argument == "--allow-write" {
                    policy.allow_write = true;
                } else if let Some(value) = argument.strip_prefix("--approval-failpoint=") {
                    policy.approval_failpoint = Some(ApprovalFailpoint::parse(value)?);
                } else if let Some(value) = argument.strip_prefix("--prompt-failpoint=") {
                    policy.prompt_failpoint = Some(PromptFailpoint::parse(value)?);
                } else {
                    return Err(SpikeError::Protocol(format!(
                        "unknown app-server option: {argument}"
                    )));
                }
            }
            let store = Store::create(database)?;
            let executable = std::env::current_exe()?;
            let core = SpikeCore::new(store, executable, policy);
            serve_jsonl(
                BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
                core,
            )
            .await
        }
        _ => Err(SpikeError::Protocol(
            "M0 spike only; use app-server <disposable-db>".to_owned(),
        )),
    }
}
