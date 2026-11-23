//! The Control channel over a real socket: a command, a Read command, a Batch, and everything
//! a hostile client can send.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use metriccraft_engine::App;
use metriccraft_engine::control::{MAX_LINE, control_file};
use metriccraft_ui_egui::control::Server;
use metriccraft_ui_egui::worker::Engine;
use serde_json::{Value, json};

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

#[cfg(test)]
impl Client {
    fn connect(port: u16) -> Client {
        let writer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Client {
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
        }
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
    }

    fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
        }
    }

    fn call(&mut self, request: &Value) -> Value {
        let mut line = request.to_string();
        line.push('\n');
        self.send_raw(line.as_bytes());
        self.read().unwrap()
    }
}

fn error_kind(reply: &Value) -> Option<&str> {
    reply["error"]["kind"].as_str()
}

struct Session {
    // Dropped (and its file removed) before the engine's worker is let go.
    server: Server,
    _engine: Engine,
    dir: std::path::PathBuf,
}

#[cfg(test)]
fn start(name: &str) -> Session {
    let dir = std::env::temp_dir().join(format!("mcraft-ctl-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(App::new(), || {});
    let server = Server::start(engine.control_link(), Some(&dir)).unwrap();
    Session {
        server,
        _engine: engine,
        dir,
    }
}

#[test]
fn runs_a_command_a_read_command_and_a_batch() {
    let s = start("run");
    let t = s.server.token().to_string();
    let mut c = Client::connect(s.server.port());

    let r =
        c.call(&json!({"token": t, "id": 1, "run": "insert.newPage", "params": {"name": "Q3"}}));
    assert_eq!((r["ok"].as_bool(), r["id"].as_i64()), (Some(true), Some(1)));

    // A Read command sees the change the command made.
    let r = c.call(&json!({"token": t, "id": "d", "run": "document.describe"}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert!(r["result"].to_string().contains("Q3"), "{r}");

    // A Batch: all or nothing, one undo step.
    let r = c.call(&json!({"token": t, "run": "batch", "params": {"commands": [
        {"run": "insert.newPage", "params": {"name": "A"}},
        {"run": "insert.newPage", "params": {"name": "B"}},
    ]}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let r = c.call(&json!({"token": t, "run": "batch", "params": {"commands": [
        {"run": "insert.newPage", "params": {"name": "C"}},
        {"run": "no.such.command"},
    ]}}));
    assert_eq!(error_kind(&r), Some("invalid_params"), "{r}");
    let r = c.call(&json!({"token": t, "run": "document.describe"}));
    let text = r["result"].to_string();
    assert!(
        text.contains("\"A\"") && text.contains("\"B\"") && !text.contains("\"C\""),
        "{r}"
    );

    // Errors are `{kind, message}` and the connection carries on.
    let r = c.call(&json!({"token": t, "run": "no.such.command"}));
    assert_eq!(error_kind(&r), Some("unknown_command"));
    assert!(r["error"]["message"].is_string());
    let r = c.call(&json!({"token": t, "run": "insert.newPage", "params": {"name": 5}}));
    assert_eq!(error_kind(&r), Some("invalid_params"));
    let r = c.call(&json!({"token": t, "run": "document.describe"}));
    assert_eq!(r["ok"], json!(true));
}

#[test]
fn rejects_a_wrong_token_and_changes_nothing() {
    let s = start("token");
    let mut c = Client::connect(s.server.port());
    let r = c.call(
        &json!({"token": "wrong", "id": 9, "run": "insert.newPage", "params": {"name": "Evil"}}),
    );
    assert_eq!(
        (error_kind(&r), r["id"].as_i64()),
        (Some("bad_token"), Some(9))
    );
    assert!(
        c.read().is_none(),
        "the connection is closed after a bad token"
    );
    let mut c = Client::connect(s.server.port());
    let r = c.call(&json!({"run": "document.describe"}));
    assert_eq!(error_kind(&r), Some("bad_token"));
    let mut c = Client::connect(s.server.port());
    let r = c.call(&json!({"token": s.server.token(), "run": "document.describe"}));
    assert!(!r["result"].to_string().contains("Evil"), "{r}");
}

#[test]
fn survives_bad_json_and_oversized_lines() {
    let s = start("hostile");
    let t = s.server.token().to_string();
    let mut c = Client::connect(s.server.port());
    for bad in [
        &b"{not json\n"[..],
        b"[1,2]\n",
        b"\xff\xfe\n",
        b"\n",
        b"null\n",
    ] {
        c.send_raw(bad);
        let r = c.read().unwrap();
        assert!(
            matches!(error_kind(&r), Some("bad_json" | "bad_request")),
            "{r}"
        );
    }
    let r = c.call(&json!({"token": t, "run": "document.describe"}));
    assert_eq!(r["ok"], json!(true));

    // Past the limit with no newline in sight: refused, connection closed, server fine.
    let mut big = Client::connect(s.server.port());
    big.send_raw(&vec![b'a'; MAX_LINE + 10]);
    let r = big.read().unwrap();
    assert_eq!(error_kind(&r), Some("too_large"));
    assert!(big.read().is_none());

    let mut again = Client::connect(s.server.port());
    let r = again.call(&json!({"token": t, "run": "document.describe"}));
    assert_eq!(r["ok"], json!(true));
}

#[test]
fn control_file_lives_and_dies_with_the_server() {
    let s = start("file");
    let path = control_file(&s.dir, std::process::id());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["port"].as_u64(), Some(u64::from(s.server.port())));
    assert_eq!(v["token"].as_str(), Some(s.server.token()));
    assert_eq!(s.server.token().len(), 32);
    let dir = s.dir.clone();
    drop(s);
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(dir);
}
