//! M6 trusted-core lifecycle primitives.
//!
//! The provider-neutral agent loop arrives in M7.  This crate currently owns
//! recovery and supervised worker process lifecycle only.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;

use cool_security::sanitize_environment;
use cool_state::{DurableStore, StoreError, WorkerStatus};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct WorkerSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub environment: HashMap<String, String>,
    pub allowed_secret_environment: BTreeSet<String>,
}

impl WorkerSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            environment: HashMap::new(),
            allowed_secret_environment: BTreeSet::new(),
        }
    }
}

#[derive(Debug)]
pub enum SupervisorError {
    Store(StoreError),
    Io(std::io::Error),
    AlreadyRunning,
    NotRunning,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "worker store error: {error}"),
            Self::Io(error) => write!(formatter, "worker process error: {error}"),
            Self::AlreadyRunning => formatter.write_str("worker is already running"),
            Self::NotRunning => formatter.write_str("worker is not running"),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<StoreError> for SupervisorError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<std::io::Error> for SupervisorError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

struct WorkerEntry {
    child: Child,
    spec: WorkerSpec,
    run_id: Option<String>,
    generation: u64,
}

#[derive(Clone)]
pub struct WorkerSupervisor {
    store: DurableStore,
    workers: Arc<Mutex<HashMap<String, WorkerEntry>>>,
}

impl WorkerSupervisor {
    pub fn new(store: DurableStore) -> Self {
        Self {
            store,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start(
        &self,
        worker_id: &str,
        run_id: Option<&str>,
        spec: WorkerSpec,
    ) -> Result<u64, SupervisorError> {
        let mut workers = self.workers.lock().await;
        if workers.contains_key(worker_id) {
            return Err(SupervisorError::AlreadyRunning);
        }
        let generation = self.store.begin_worker_generation(worker_id, run_id)?;
        let child = match spawn(&spec) {
            Ok(child) => child,
            Err(error) => {
                self.store.record_worker_state(
                    worker_id,
                    run_id,
                    WorkerStatus::Failed,
                    generation,
                    Some(&error.to_string()),
                )?;
                return Err(error.into());
            }
        };
        self.store.record_worker_state(
            worker_id,
            run_id,
            WorkerStatus::Running,
            generation,
            None,
        )?;
        workers.insert(
            worker_id.to_owned(),
            WorkerEntry {
                child,
                spec,
                run_id: run_id.map(str::to_owned),
                generation,
            },
        );
        Ok(generation)
    }

    pub async fn poll(&self, worker_id: &str) -> Result<Option<ExitStatus>, SupervisorError> {
        let mut workers = self.workers.lock().await;
        let entry = workers
            .get_mut(worker_id)
            .ok_or(SupervisorError::NotRunning)?;
        let status = entry.child.try_wait()?;
        if let Some(status) = status {
            let entry = workers.remove(worker_id).expect("entry exists");
            let (worker_status, detail) = if status.success() {
                (WorkerStatus::Stopped, None)
            } else {
                (WorkerStatus::Failed, Some(format!("exit status {status}")))
            };
            self.store.record_worker_state(
                worker_id,
                entry.run_id.as_deref(),
                worker_status,
                entry.generation,
                detail.as_deref(),
            )?;
        }
        Ok(status)
    }

    pub async fn kill_and_restart(&self, worker_id: &str) -> Result<u64, SupervisorError> {
        let (spec, run_id, generation) = {
            let mut workers = self.workers.lock().await;
            let entry = workers
                .get_mut(worker_id)
                .ok_or(SupervisorError::NotRunning)?;
            entry.child.kill().await?;
            entry.child.wait().await?;
            let result = (entry.spec.clone(), entry.run_id.clone(), entry.generation);
            workers.remove(worker_id);
            result
        };
        self.store.record_worker_state(
            worker_id,
            run_id.as_deref(),
            WorkerStatus::Failed,
            generation,
            Some("killed_by_supervisor"),
        )?;
        self.start(worker_id, run_id.as_deref(), spec).await
    }

    pub async fn stop(&self, worker_id: &str) -> Result<(), SupervisorError> {
        let (run_id, generation) = {
            let mut workers = self.workers.lock().await;
            let entry = workers
                .get_mut(worker_id)
                .ok_or(SupervisorError::NotRunning)?;
            entry.child.kill().await?;
            entry.child.wait().await?;
            let result = (entry.run_id.clone(), entry.generation);
            workers.remove(worker_id);
            result
        };
        self.store.record_worker_state(
            worker_id,
            run_id.as_deref(),
            WorkerStatus::Stopped,
            generation,
            None,
        )?;
        Ok(())
    }

    /// Starts a new generation after a core restart. Persisted worker state is
    /// never interpreted as proof that an OS process is still attached.
    pub async fn recover(
        &self,
        worker_id: &str,
        run_id: Option<&str>,
        spec: WorkerSpec,
    ) -> Result<u64, SupervisorError> {
        self.start(worker_id, run_id, spec).await
    }
}

fn spawn(spec: &WorkerSpec) -> Result<Child, std::io::Error> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    let environment = sanitize_environment(
        spec.environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        &spec.allowed_secret_environment,
    );
    command.env_clear().envs(environment);
    command.spawn()
}
