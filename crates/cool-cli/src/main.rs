use std::env;
use std::path::PathBuf;

use cool_app_server::{AppServer, ServerConfig, capabilities};
use serde_json::json;

#[tokio::main]
async fn main() {
    if let Err((code, message)) = run().await {
        eprintln!(
            "{}",
            serde_json::to_string(&message).expect("error JSON serializes")
        );
        std::process::exit(code);
    }
}

async fn run() -> Result<(), (i32, serde_json::Value)> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };
    match command.as_str() {
        "app-server" => {
            let mut transport = "stdio".to_owned();
            let mut endpoint: Option<PathBuf> = None;
            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--transport" => {
                        transport = args
                            .next()
                            .ok_or_else(|| usage("missing transport value"))?;
                    }
                    "--endpoint" => {
                        endpoint = Some(PathBuf::from(
                            args.next().ok_or_else(|| usage("missing endpoint value"))?,
                        ));
                    }
                    _ => return Err(usage("unknown app-server argument")),
                }
            }
            let server = AppServer::new(ServerConfig::default());
            match transport.as_str() {
                "stdio" if endpoint.is_none() => server
                    .serve_stdio()
                    .await
                    .map_err(|error| runtime("app_server_failed", &error.to_string())),
                "local" => {
                    let endpoint =
                        endpoint.ok_or_else(|| usage("local transport needs endpoint"))?;
                    server
                        .serve_local(&endpoint)
                        .await
                        .map_err(|error| runtime("local_transport_failed", &error.to_string()))
                }
                _ => Err(usage("transport must be stdio or local")),
            }
        }
        "doctor" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": "ok",
                    "phase": "M5",
                    "runtime": "rust-app-server-skeleton",
                    "protocolVersion": 1,
                    "capabilities": capabilities(),
                    "durableState": false,
                    "agentLoop": false
                }))
                .expect("doctor JSON serializes")
            );
            Ok(())
        }
        "serve" | "run" => Err((
            2,
            json!({
                "coolCode": "m5_route_not_implemented",
                "message": format!("{command} is routed but becomes operational in a later phase"),
                "retryable": false
            }),
        )),
        "--version" | "-V" => {
            println!("cool {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        _ => Err(usage("unknown command")),
    }
}

fn usage(message: &str) -> (i32, serde_json::Value) {
    (
        2,
        json!({"coolCode": "invalid_cli_usage", "message": message, "retryable": false}),
    )
}

fn runtime(code: &str, message: &str) -> (i32, serde_json::Value) {
    (
        1,
        json!({"coolCode": code, "message": message, "retryable": false}),
    )
}

fn print_help() {
    println!(
        "Cool Rust CLI skeleton\n\nCommands:\n  app-server [--transport stdio|local] [--endpoint PATH]\n  serve\n  run\n  doctor"
    );
}
