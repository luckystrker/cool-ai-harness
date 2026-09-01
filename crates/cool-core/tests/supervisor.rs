use std::path::PathBuf;

use cool_core::{WorkerSpec, WorkerSupervisor};
use cool_state::DurableStore;
use tokio::time::{Duration, sleep, timeout};

fn sleeping_worker() -> WorkerSpec {
    #[cfg(windows)]
    {
        let mut spec = WorkerSpec::new(PathBuf::from("cmd.exe"));
        spec.args = vec!["/C".to_owned(), "ping -n 30 127.0.0.1".to_owned()];
        spec
    }
    #[cfg(not(windows))]
    {
        let mut spec = WorkerSpec::new(PathBuf::from("sh"));
        spec.args = vec!["-c".to_owned(), "sleep 30".to_owned()];
        spec
    }
}

fn secret_probe(expect_present: bool) -> WorkerSpec {
    #[cfg(windows)]
    let mut spec = {
        let mut spec = WorkerSpec::new(PathBuf::from("cmd.exe"));
        let condition = if expect_present {
            "if defined OPENAI_API_KEY (exit /b 0) else (exit /b 9)"
        } else {
            "if defined OPENAI_API_KEY (exit /b 9) else (exit /b 0)"
        };
        spec.args = vec!["/C".to_owned(), condition.to_owned()];
        spec
    };
    #[cfg(not(windows))]
    let mut spec = {
        let mut spec = WorkerSpec::new(PathBuf::from("sh"));
        let condition = if expect_present {
            "test -n \"$OPENAI_API_KEY\""
        } else {
            "test -z \"$OPENAI_API_KEY\""
        };
        spec.args = vec!["-c".to_owned(), condition.to_owned()];
        spec
    };
    spec.environment
        .insert("OPENAI_API_KEY".to_owned(), "must-not-leak".to_owned());
    if expect_present {
        spec.allowed_secret_environment
            .insert("OPENAI_API_KEY".to_owned());
    }
    spec
}

async fn wait_for_exit(supervisor: &WorkerSupervisor, worker_id: &str) -> std::process::ExitStatus {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = supervisor.poll(worker_id).await.unwrap() {
                return status;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker exits")
}

#[tokio::test]
async fn kill_restart_and_core_restart_advance_durable_generation() {
    let store = DurableStore::in_memory().unwrap();
    let supervisor = WorkerSupervisor::new(store.clone());
    assert_eq!(
        supervisor
            .start("worker-1", None, sleeping_worker())
            .await
            .unwrap(),
        1
    );
    assert_eq!(supervisor.kill_and_restart("worker-1").await.unwrap(), 2);
    supervisor.stop("worker-1").await.unwrap();

    let after_core_restart = WorkerSupervisor::new(store.clone());
    assert_eq!(
        after_core_restart
            .recover("worker-1", None, sleeping_worker())
            .await
            .unwrap(),
        3
    );
    after_core_restart.stop("worker-1").await.unwrap();
    assert_eq!(store.worker_generation("worker-1").unwrap(), Some(3));
}

#[tokio::test]
async fn worker_environment_is_secret_filtered_unless_explicitly_allowed() {
    let store = DurableStore::in_memory().unwrap();
    let supervisor = WorkerSupervisor::new(store);
    supervisor
        .start("filtered", None, secret_probe(false))
        .await
        .unwrap();
    assert!(wait_for_exit(&supervisor, "filtered").await.success());
    supervisor
        .start("allowed", None, secret_probe(true))
        .await
        .unwrap();
    assert!(wait_for_exit(&supervisor, "allowed").await.success());
}
