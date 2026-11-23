//! The Control channel's server (native only): a TCP socket on 127.0.0.1, a random port and a
//! per-session token, feeding the worker's queue. The request and reply types, the line limit
//! and the control file are in `metriccraft_engine::control`; this is only the sockets.
//!
//! It is off unless asked for (`metriccraft --control`, or `"control": true` in the
//! preferences). While it runs, `control-<pid>.json` in the runtime folder holds the port and
//! token for local programs; the file is removed when the [`Server`] is dropped, and files of
//! sessions that died without removing theirs are dropped when a server starts.

use std::io::{self, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use metriccraft_engine::control::{
    self, Kind, Line, MAX_LINE, Reply, Request, read_line, remove_control_file, write_control_file,
};

use crate::worker::ControlLink;

/// Connections served at once; more are closed at once.
const MAX_CONNECTIONS: usize = 16;
/// How long a connection may sit silent before it is closed.
const IDLE: Duration = Duration::from_secs(120);
/// How long a reply may take to be taken by the peer.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// A running Control channel server.
pub struct Server {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    file: Option<(PathBuf, u32)>,
}

impl Server {
    /// Starts the server, writing its control file into `dir` (normally
    /// [`metriccraft_engine::control::runtime_dir`]) after dropping stale ones there. With no
    /// `dir` it still serves, but nothing can find it but the caller, who reads
    /// [`Server::port`] and [`Server::token`].
    pub fn start(link: ControlLink, dir: Option<&Path>) -> io::Result<Server> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = new_token();
        let pid = std::process::id();
        let file = match dir {
            Some(dir) => {
                control::drop_stale(dir, pid, listening);
                write_control_file(dir, pid, port, &token)?;
                Some((dir.to_path_buf(), pid))
            }
            None => None,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let server = Server {
            port,
            token: token.clone(),
            stop: Arc::clone(&stop),
            file,
        };
        // ponytail: unnamed detached threads; they end with `stop` or the process.
        std::thread::spawn(move || accept_loop(&listener, &link, &token, &stop));
        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some((dir, pid)) = &self.file {
            remove_control_file(dir, *pid);
        }
    }
}

/// Whether something accepts connections on `port` of this machine.
fn listening(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn accept_loop(listener: &TcpListener, link: &ControlLink, token: &str, stop: &Arc<AtomicBool>) {
    let open = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if open.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    continue; // dropped: closed
                }
                open.fetch_add(1, Ordering::Relaxed);
                let (link, token, open, stop) = (
                    link.clone(),
                    token.to_string(),
                    Arc::clone(&open),
                    Arc::clone(stop),
                );
                std::thread::spawn(move || {
                    let _ = serve(stream, &link, &token, &stop);
                    open.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// One connection: a reply for every line, until the peer leaves or a line can't be trusted.
fn serve(stream: TcpStream, link: &ControlLink, token: &str, stop: &AtomicBool) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while !stop.load(Ordering::Relaxed) {
        let (reply, keep) = match read_line(&mut reader, MAX_LINE)? {
            Line::End => return Ok(()),
            Line::TooLong => (
                Reply::error(
                    None,
                    Kind::TooLarge,
                    format!("a request line is at most {MAX_LINE} bytes"),
                ),
                false,
            ),
            Line::Text(line) => match Request::parse(&line, token) {
                Ok(request) => (link.call(request), true),
                // A wrong token ends the connection: no guessing at speed.
                Err(reply) => {
                    let keep = !matches!(reply.outcome, Err((Kind::BadToken, _)));
                    (reply, keep)
                }
            },
        };
        writer.write_all(reply.to_line().as_bytes())?;
        if !keep {
            return Ok(());
        }
    }
    Ok(())
}

/// 128 random bits as hex: the operating system's randomness where there is a device for it,
/// else the randomly keyed hashers of `std`, mixed with the time and process id.
fn new_token() -> String {
    let mut bytes = [0u8; 16];
    #[cfg(unix)]
    let from_os = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    #[cfg(not(unix))]
    let from_os = false;
    if !from_os {
        use std::hash::{BuildHasher, Hasher};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            // Each `RandomState` is keyed from the operating system.
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(now);
            h.write_u32(std::process::id());
            h.write_usize(i);
            let word = h.finish().to_le_bytes();
            for (dst, src) in chunk.iter_mut().zip(word) {
                *dst = src;
            }
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
