//! The Control channel's requests and replies (glossary: Control channel; Origin Control).
//!
//! A local program talks to a running window over a TCP socket (the server lives in
//! `metriccraft-ui-egui`); this module is everything that is not sockets, so it is tested
//! without any. One request and one reply per line of JSON:
//!
//! ```text
//! -> {"token": "…", "id": 7, "run": "insert.newPage", "params": {"name": "Q3"}}
//! <- {"id": 7, "ok": true, "result": null}
//! <- {"id": 7, "ok": false, "error": {"kind": "invalid_params", "message": "…"}}
//! ```
//!
//! `run` is any command id: an edit, a Read command (`dax.evaluate`) or `batch`. `id` is
//! optional, any JSON value, and echoed back. A line is at most [`MAX_LINE`] bytes. The token
//! is per session; the port and token are written to `control-<pid>.json` in [`runtime_dir`].
//! Every failure is a reply `{kind, message}` and the channel carries on, except a line that is
//! too long, which closes the connection (the rest of the line can't be told from the next).

use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::{App, Error, Origin};

/// The longest request line, in bytes, newline excluded.
pub const MAX_LINE: usize = 1 << 20;

/// The most `control-*.json` files looked at when dropping stale ones.
const MAX_FILES: usize = 256;

/// Why a request failed, as the `kind` of the reply's error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The line is not JSON.
    BadJson,
    /// JSON, but not `{"token", "run", "params"?, "id"?}`.
    BadRequest,
    /// The token is missing or wrong.
    BadToken,
    /// The line is longer than [`MAX_LINE`].
    TooLarge,
    /// The engine is not running any more.
    Stopped,
    /// Command: no such id.
    UnknownCommand,
    /// Command: not available now.
    Disabled,
    /// Command: params missing, of the wrong type or out of range.
    InvalidParams,
    /// Command: a Batch step failed; nothing was changed.
    Batch,
    /// Command: a Read command failed.
    Read,
    /// Command: the data, model or file layer failed.
    Failed,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadJson => "bad_json",
            Self::BadRequest => "bad_request",
            Self::BadToken => "bad_token",
            Self::TooLarge => "too_large",
            Self::Stopped => "stopped",
            Self::UnknownCommand => "unknown_command",
            Self::Disabled => "disabled",
            Self::InvalidParams => "invalid_params",
            Self::Batch => "batch",
            Self::Read => "read",
            Self::Failed => "failed",
        }
    }
}

impl From<&Error> for Kind {
    fn from(e: &Error) -> Self {
        match e {
            Error::UnknownCommand(_) => Self::UnknownCommand,
            Error::Disabled(_) => Self::Disabled,
            Error::InvalidParams { .. } => Self::InvalidParams,
            Error::Batch { .. } => Self::Batch,
            Error::Read { .. } => Self::Read,
            _ => Self::Failed,
        }
    }
}

/// A request that passed the checks of [`Request::parse`].
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// Echoed in the reply.
    pub id: Option<Value>,
    /// The command to run.
    pub run: String,
    pub params: Value,
}

/// A reply: the result of the command, or an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub id: Option<Value>,
    pub outcome: Result<Value, (Kind, String)>,
}

impl Reply {
    pub fn error(id: Option<Value>, kind: Kind, message: impl Into<String>) -> Self {
        Reply {
            id,
            outcome: Err((kind, message.into())),
        }
    }

    /// The reply as one JSON object (no newline).
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        if let Some(id) = &self.id {
            out.insert("id".into(), id.clone());
        }
        match &self.outcome {
            Ok(result) => {
                out.insert("ok".into(), Value::Bool(true));
                out.insert("result".into(), result.clone());
            }
            Err((kind, message)) => {
                out.insert("ok".into(), Value::Bool(false));
                out.insert(
                    "error".into(),
                    json!({"kind": kind.as_str(), "message": message}),
                );
            }
        }
        Value::Object(out)
    }

    /// The reply as a line, newline included.
    pub fn to_line(&self) -> String {
        let mut line = self.to_json().to_string();
        line.push('\n');
        line
    }
}

/// Whether `given` is `expected`, comparing every byte so the time taken says nothing about
/// how much of a guess was right. An empty `expected` matches nothing.
pub fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    let mut diff = a.len() ^ b.len();
    for (i, x) in a.iter().enumerate() {
        diff |= usize::from(x ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0 && !a.is_empty()
}

impl Request {
    /// Reads a request line: valid JSON, the right `token` (checked before anything else is
    /// looked at), a `run` that is a string, `params` (default `{}`).
    pub fn parse(line: &[u8], token: &str) -> Result<Request, Reply> {
        if line.len() > MAX_LINE {
            return Err(Reply::error(None, Kind::TooLarge, too_large()));
        }
        let value: Value = serde_json::from_slice(line)
            .map_err(|e| Reply::error(None, Kind::BadJson, format!("not JSON: {e}")))?;
        let bad = |message: &str| Reply::error(None, Kind::BadRequest, message);
        let Value::Object(mut obj) = value else {
            return Err(bad("a request is a JSON object"));
        };
        let id = obj.remove("id");
        let reply = |kind, message: &str| Reply::error(id.clone(), kind, message);
        match obj.get("token").and_then(Value::as_str) {
            Some(given) if token_matches(token, given) => {}
            _ => return Err(reply(Kind::BadToken, "missing or wrong token")),
        }
        let Some(Value::String(run)) = obj.remove("run") else {
            return Err(reply(Kind::BadRequest, "`run` must be a command id"));
        };
        let params = obj.remove("params").unwrap_or_else(|| json!({}));
        Ok(Request { id, run, params })
    }
}

fn too_large() -> String {
    format!("a request line is at most {MAX_LINE} bytes")
}

/// Runs the request as [`Origin::Control`].
pub fn execute(app: &mut App, req: &Request) -> Reply {
    let outcome = app
        .run_as(Origin::Control, &req.run, &req.params)
        .map_err(|e| (Kind::from(&e), e.to_string()));
    Reply {
        id: req.id.clone(),
        outcome,
    }
}

/// What reading a line gave.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// One line, without its newline (and carriage return).
    Text(Vec<u8>),
    /// The line is longer than the limit; the rest of it was not read.
    TooLong,
    /// The peer closed the connection.
    End,
}

/// Reads one line of at most `max` bytes without ever holding more than `max + 1`.
pub fn read_line<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Line> {
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let mut buf = Vec::new();
    reader.by_ref().take(limit).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Ok(Line::End);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        // A newline in the last byte of the allowance is still a line of `max` bytes.
        return Ok(Line::Text(buf));
    }
    if buf.len() > max {
        return Ok(Line::TooLong);
    }
    // The peer closed after a last line with no newline.
    Ok(Line::Text(buf))
}

// ---- the control file ----

/// The per-user folder for runtime files: `$XDG_RUNTIME_DIR/metriccraft` on Linux (else the
/// cache folder's `metriccraft/run`), `~/Library/Caches/metriccraft/run` on macOS,
/// `%LOCALAPPDATA%\metriccraft\run` on Windows. `None` where there is none (the web build, or
/// no home directory).
pub fn runtime_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    #[cfg(windows)]
    let base = var("LOCALAPPDATA").map(|b| b.join("metriccraft").join("run"));
    #[cfg(target_os = "macos")]
    let base = var("HOME").map(|h| h.join("Library/Caches/metriccraft/run"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = var("XDG_RUNTIME_DIR")
        .map(|d| d.join("metriccraft"))
        .or_else(|| {
            var("XDG_CACHE_HOME")
                .or_else(|| var("HOME").map(|h| h.join(".cache")))
                .map(|d| d.join("metriccraft/run"))
        });
    base
}

/// The control file of process `pid` in `dir`.
pub fn control_file(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("control-{pid}.json"))
}

/// Writes `control-<pid>.json` (`{"pid", "port", "token"}`) into `dir`, which is made if
/// missing. On Unix the folder is created user-only (0700) and the file is created user-only
/// (0600) before anything is written to it, so the token is never readable by others; on
/// Windows both inherit the access of the per-user [`runtime_dir`]. Replaces an older file of
/// the same pid.
pub fn write_control_file(dir: &Path, pid: u32, port: u16, token: &str) -> io::Result<PathBuf> {
    create_private_dir(dir)?;
    let path = control_file(dir, pid);
    // A stale file of a reused pid goes first, so the new one is created with our mode.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let body = json!({"pid": pid, "port": port, "token": token}).to_string();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    io::Write::write_all(&mut options.open(&path)?, body.as_bytes())?;
    Ok(path)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Removes the control file of `pid`; one that is already gone is fine.
pub fn remove_control_file(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(control_file(dir, pid));
}

/// Reads a control file: `(pid, port)`; `None` if it is not one of ours.
fn read_control_file(path: &Path) -> Option<(u32, u16)> {
    // Ours is tiny; anything bigger is not.
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_string(&mut text)
        .ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    Some((pid, port))
}

/// Deletes the `control-<pid>.json` files in `dir` whose session is gone: `alive(port)` says
/// whether something still listens there. Files of `keep_pid` (this process) stay; files that
/// are not readable as ours are removed too, as they can't be used. Returns how many went.
pub fn drop_stale(dir: &Path, keep_pid: u32, alive: impl Fn(u16) -> bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut dropped = 0usize;
    for entry in entries.flatten().take(MAX_FILES) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("control-") && name.ends_with(".json")) {
            continue;
        }
        let path = entry.path();
        let stale = match read_control_file(&path) {
            Some((pid, _)) if pid == keep_pid => false,
            Some((_, port)) => !alive(port),
            None => true,
        };
        if stale && std::fs::remove_file(&path).is_ok() {
            dropped = dropped.saturating_add(1);
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn kind_of(r: Result<Request, Reply>) -> Kind {
        match r {
            Err(Reply {
                outcome: Err((k, _)),
                ..
            }) => k,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_request() {
        let r = Request::parse(br#"{"token":"t","id":7,"run":"x","params":{"a":1}}"#, "t").unwrap();
        assert_eq!(r.id, Some(json!(7)));
        assert_eq!(r.run, "x");
        assert_eq!(r.params, json!({"a": 1}));
        let r = Request::parse(br#"{"token":"t","run":"x"}"#, "t").unwrap();
        assert_eq!((r.id, r.params), (None, json!({})));
    }

    #[test]
    fn rejects_bad_requests() {
        assert_eq!(kind_of(Request::parse(b"{nope", "t")), Kind::BadJson);
        assert_eq!(kind_of(Request::parse(b"[1]", "t")), Kind::BadRequest);
        assert_eq!(kind_of(Request::parse(b"{}", "t")), Kind::BadToken);
        assert_eq!(
            kind_of(Request::parse(br#"{"token":"u","run":"x"}"#, "t")),
            Kind::BadToken
        );
        assert_eq!(
            kind_of(Request::parse(br#"{"token":5,"run":"x"}"#, "t")),
            Kind::BadToken
        );
        assert_eq!(
            kind_of(Request::parse(br#"{"token":"t","run":5}"#, "t")),
            Kind::BadRequest
        );
        // The token is checked before the rest: a bad one never learns about `run`.
        assert_eq!(
            kind_of(Request::parse(br#"{"run":5}"#, "t")),
            Kind::BadToken
        );
        assert_eq!(
            kind_of(Request::parse(&vec![b' '; MAX_LINE + 1], "t")),
            Kind::TooLarge
        );
        // An empty expected token matches nothing, not even an empty one.
        assert!(!token_matches("", ""));
        assert!(
            token_matches("ab", "ab") && !token_matches("ab", "abc") && !token_matches("ab", "a")
        );
    }

    #[test]
    fn replies_carry_kind_and_message() {
        let r = Reply::error(Some(json!("q")), Kind::BadToken, "no");
        assert_eq!(
            r.to_json(),
            json!({"id": "q", "ok": false, "error": {"kind": "bad_token", "message": "no"}})
        );
        let r = Reply {
            id: None,
            outcome: Ok(json!(1)),
        };
        assert_eq!(r.to_json(), json!({"ok": true, "result": 1}));
        assert!(r.to_line().ends_with('\n'));
    }

    #[test]
    fn executes_as_control_and_maps_errors() {
        let mut app = App::new();
        let ok = |run: &str, params: Value| Request {
            id: Some(json!(1)),
            run: run.into(),
            params,
        };
        let r = execute(&mut app, &ok("insert.newPage", json!({"name": "Q3"})));
        assert_eq!(r.outcome, Ok(Value::Null));
        assert!(
            app.undo_label()
                .is_some_and(|l| l.starts_with("Control channel"))
        );
        let kind = |r: Reply| r.outcome.map_err(|e| e.0);
        assert_eq!(
            kind(execute(&mut app, &ok("nope", json!({})))),
            Err(Kind::UnknownCommand)
        );
        assert_eq!(
            kind(execute(&mut app, &ok("insert.newPage", json!({"name": 5})))),
            Err(Kind::InvalidParams)
        );
        assert_eq!(
            kind(execute(
                &mut app,
                &ok("batch", json!({"commands": [{"run": "nope"}]}))
            )),
            Err(Kind::InvalidParams)
        );
    }

    #[test]
    fn lines_are_bounded() {
        let read = |bytes: &[u8], max| read_line(&mut Cursor::new(bytes.to_vec()), max).unwrap();
        assert_eq!(read(b"abc\r\nrest", 10), Line::Text(b"abc".to_vec()));
        assert_eq!(read(b"abcd\n", 4), Line::Text(b"abcd".to_vec()));
        assert_eq!(read(b"abcde\n", 4), Line::TooLong);
        assert_eq!(read(b"abcdefgh", 4), Line::TooLong);
        assert_eq!(read(b"tail", 10), Line::Text(b"tail".to_vec()));
        assert_eq!(read(b"", 10), Line::End);
    }

    #[test]
    fn control_file_is_private_and_stale_ones_go() {
        let dir = std::env::temp_dir().join(format!("mcraft-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = write_control_file(&dir, 10, 1111, "tok").unwrap();
        write_control_file(&dir, 11, 2222, "tok").unwrap();
        write_control_file(&dir, 12, 3333, "tok").unwrap();
        std::fs::write(control_file(&dir, 13), b"garbage").unwrap();
        std::fs::write(dir.join("other.json"), b"{}").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&me).unwrap()).unwrap();
        assert_eq!(
            (v["port"].as_u64(), v["token"].as_str()),
            (Some(1111), Some("tok"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&me), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        // pid 10 is this process; port 2222 is alive, 3333 is not, 13 is not a control file.
        assert_eq!(drop_stale(&dir, 10, |p| p == 2222), 2);
        assert!(control_file(&dir, 10).exists() && control_file(&dir, 11).exists());
        assert!(!control_file(&dir, 12).exists() && !control_file(&dir, 13).exists());
        assert!(dir.join("other.json").exists());
        remove_control_file(&dir, 10);
        remove_control_file(&dir, 10);
        assert!(!control_file(&dir, 10).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
