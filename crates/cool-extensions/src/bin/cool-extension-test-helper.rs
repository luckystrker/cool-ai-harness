use std::io::{self, BufRead as _, Read as _, Write as _};

use serde_json::{Value, json};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "mcp" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            match request.get("method").and_then(Value::as_str) {
                Some("initialize") => Some(
                    json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"test","version":"1"}}}),
                ),
                Some("tools/list") => Some(
                    json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}),
                ),
                Some("tools/call") => Some(
                    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}),
                ),
                _ => None,
            }
        }),
        "mcp-bad-version" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            Some(
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2099-01-01","capabilities":{}}}),
            )
        }),
        "worker" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request.get("method").and_then(Value::as_str);
            let result = if method == Some("handshake") {
                json!({"protocolVersion":1,"capabilities":["request","cancel","heartbeat","shutdown","deadlines","structured_errors",format!("{}_protocol", request["params"]["adapter"].as_str().unwrap())]})
            } else {
                let translated = json!({
                    "receivedMethod": request.get("method"),
                    "deadlineUnixMs": request.get("deadlineUnixMs"),
                    "params": request.get("params")
                });
                match method {
                    Some("codex.request") => json!({"output": translated}),
                    Some("claude.request") => json!({"content": translated}),
                    _ => translated,
                }
            };
            Some(json!({"id":id,"ok":true,"result":result,"error":null}))
        }),
        "worker-crash-request" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            if request.get("method").and_then(Value::as_str) == Some("handshake") {
                Some(
                    json!({"id":id,"ok":true,"result":{"protocolVersion":1,"capabilities":["request","cancel","heartbeat","shutdown","deadlines","structured_errors",format!("{}_protocol", request["params"]["adapter"].as_str().unwrap())]},"error":null}),
                )
            } else {
                std::process::exit(19)
            }
        }),
        "worker-slow" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            if request.get("method").and_then(Value::as_str) == Some("handshake") {
                Some(
                    json!({"id":id,"ok":true,"result":{"protocolVersion":1,"capabilities":["request","cancel","heartbeat","shutdown","deadlines","structured_errors",format!("{}_protocol", request["params"]["adapter"].as_str().unwrap())]},"error":null}),
                )
            } else {
                std::thread::sleep(std::time::Duration::from_secs(2));
                Some(json!({"id":id,"ok":true,"result":null,"error":null}))
            }
        }),
        "worker-error" => json_lines(|request| {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            if request.get("method").and_then(Value::as_str) == Some("handshake") {
                Some(
                    json!({"id":id,"ok":true,"result":{"protocolVersion":1,"capabilities":["request","cancel","heartbeat","shutdown","deadlines","structured_errors",format!("{}_protocol", request["params"]["adapter"].as_str().unwrap())]},"error":null}),
                )
            } else {
                Some(
                    json!({"id":id,"ok":false,"result":null,"error":{"code":"fixture_denied","message":"denied","retryable":false,"data":null}}),
                )
            }
        }),
        "oversized-mcp" => {
            let mut lines = io::stdin().lock().lines();
            let initialize: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            println!(
                "{}",
                json!({"jsonrpc":"2.0","id":initialize["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{}}})
            );
            io::stdout().flush().unwrap();
            let _ = lines.next();
            let _ = lines.next();
            io::stdout().write_all(&vec![b'x'; 1_048_577]).unwrap();
            io::stdout().flush().unwrap();
        }
        "crash" => std::process::exit(17),
        "hook" => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .expect("read hook input");
            println!(
                "{}",
                json!({"received": serde_json::from_str::<Value>(&input).expect("hook JSON")})
            );
        }
        _ => std::process::exit(2),
    }
}

fn json_lines(mut handler: impl FnMut(&Value) -> Option<Value>) {
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.expect("line")).expect("request JSON");
        if let Some(response) = handler(&request) {
            println!("{response}");
            io::stdout().flush().expect("flush response");
        }
    }
}
