/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::backend::Backend;
use crate::server::accept_each;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[derive(Deserialize)]
struct Request {
    cmd: String,
    #[serde(default)]
    args: Value,
}

pub fn dispatch<B: Backend>(backend: &B, line: &str) -> String {
    let reply = match serde_json::from_str::<Request>(line) {
        Err(e) => Err(anyhow::anyhow!("bad request: {e}")),
        Ok(req) => generic(backend, &req).unwrap_or_else(|| {
            backend
                .control(&req.cmd, &req.args)
                .unwrap_or_else(|| Err(anyhow::anyhow!("unknown command {}", req.cmd)))
        }),
    };

    match reply {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
    }
    .to_string()
}

fn generic<B: Backend>(backend: &B, req: &Request) -> Option<anyhow::Result<Value>> {
    let journal = backend.journal();
    let since = req.args.get("since").and_then(Value::as_u64).unwrap_or(0);
    let value = match req.cmd.as_str() {
        "journal" => serde_json::to_value(journal.since(since)),
        "builds" => serde_json::to_value(
            journal
                .since(since)
                .into_iter()
                .filter(|e| e.op == "build_derivation")
                .collect::<Vec<_>>(),
        ),
        "violations" => serde_json::to_value(journal.violations()),
        "latency" => serde_json::to_value(journal.stats()),
        "reset-journal" => {
            journal.reset();
            Ok(Value::Null)
        }
        _ => return None,
    };

    Some(value.map_err(Into::into))
}

pub async fn serve_control<B: Backend>(
    backend: Arc<B>,
    listener: UnixListener,
) -> anyhow::Result<()> {
    accept_each(listener, |stream| answer(backend.clone(), stream)).await
}

async fn answer<B: Backend>(backend: Arc<B>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = dispatch(backend.as_ref(), &line);
        if write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

pub async fn request(path: &Path, cmd: &str, args: Value) -> anyhow::Result<Value> {
    let stream = UnixStream::connect(path).await?;
    let (read, mut write) = stream.into_split();
    let line = format!("{}\n", json!({ "cmd": cmd, "args": args }));
    write.write_all(line.as_bytes()).await?;
    let reply = BufReader::new(read)
        .lines()
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("control socket closed"))?;
    Ok(serde_json::from_str(&reply)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ConnInfo, NullHandler};
    use crate::journal::Journal;

    struct Echo(Journal);

    impl Backend for Echo {
        type Handler = NullHandler;

        fn journal(&self) -> &Journal {
            &self.0
        }

        fn handler(self: &Arc<Self>, _conn: ConnInfo) -> Self::Handler {
            NullHandler
        }

        fn control(&self, cmd: &str, args: &Value) -> Option<anyhow::Result<Value>> {
            (cmd == "echo").then(|| Ok(args.clone()))
        }
    }

    fn call(backend: &Echo, line: &str) -> Value {
        serde_json::from_str(&dispatch(backend, line)).expect("json reply")
    }

    #[test]
    fn generic_journal_command() {
        let backend = Echo(Journal::new());
        backend
            .0
            .start(1, "is_valid_path", vec![])
            .finish(true, None);
        let reply = call(&backend, r#"{"cmd":"journal","args":{"since":0}}"#);
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"][0]["op"], "is_valid_path");
    }

    #[test]
    fn backend_command_is_delegated() {
        let reply = call(&Echo(Journal::new()), r#"{"cmd":"echo","args":{"x":1}}"#);
        assert_eq!(reply["result"]["x"], 1);
    }

    #[test]
    fn unknown_command_and_bad_json_are_errors_not_panics() {
        let backend = Echo(Journal::new());
        assert_eq!(call(&backend, r#"{"cmd":"nope","args":{}}"#)["ok"], false);
        assert_eq!(call(&backend, "not json")["ok"], false);
    }
}
