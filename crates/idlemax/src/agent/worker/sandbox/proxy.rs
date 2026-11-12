//! Adapter: the allow-list proxy of a sandboxed worker (A13, Spec 6 Task 3). It serves one
//! Unix socket bound into that worker's sandbox, opens `CONNECT host:443` for the hosts of the
//! worker's CLI only (`host_allowed`), and answers everything else with 403 or 405. It is the
//! sandbox's only way out.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use idlemax_core::types::JobId;
use idlemax_core::worker::sandbox::host_allowed;

/// The longest request head read before the proxy gives up on a connection.
const MAX_HEAD: usize = 8 * 1024;
/// How often the accept loop looks at its stop flag.
const POLL: Duration = Duration::from_millis(200);

/// Opens a TCP connection to `host:port`; tests map names to loopback.
pub type Dial = fn(&str) -> io::Result<TcpStream>;

/// One worker's proxy; dropping it stops the loop and removes the socket.
pub struct Proxy {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Proxy {
    /// Serves `socket` for `job` with `hosts`, dialling through `dial`.
    pub fn start(socket: &Path, job: JobId, hosts: Vec<String>, dial: Dial) -> io::Result<Self> {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let hosts: Arc<[String]> = hosts.into();
        std::thread::spawn(move || accept_loop(&listener, &flag, job, &hosts, dial));
        Ok(Self {
            socket: socket.to_path_buf(),
            stop,
        })
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The real dial.
pub fn dial_tcp(target: &str) -> io::Result<TcpStream> {
    TcpStream::connect(target)
}

fn accept_loop(
    listener: &UnixListener,
    stop: &AtomicBool,
    job: JobId,
    hosts: &Arc<[String]>,
    dial: Dial,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((conn, _)) => {
                let hosts = Arc::clone(hosts);
                std::thread::spawn(move || handle(conn, job, &hosts, dial));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

fn handle(mut conn: UnixStream, job: JobId, hosts: &[String], dial: Dial) {
    let _ = conn.set_nonblocking(false);
    let Some(head) = read_head(&mut conn) else {
        return;
    };
    let first = head.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method != "CONNECT" {
        let _ = conn.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    if !host_allowed(hosts, target) {
        tracing::info!(job, target, "sandbox proxy refused a host");
        let _ = conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let Ok(upstream) = dial(target) else {
        let _ = conn.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    if conn
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_ok()
    {
        splice(conn, upstream);
    }
}

/// The request head up to the blank line, or None past `MAX_HEAD` or on a closed connection.
fn read_head(conn: &mut UnixStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD || conn.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

fn splice(conn: UnixStream, upstream: TcpStream) {
    let (Ok(mut conn_r), Ok(mut up_w)) = (conn.try_clone(), upstream.try_clone()) else {
        return;
    };
    let (mut up_r, mut conn_w) = (upstream, conn);
    let out = std::thread::spawn(move || {
        let _ = io::copy(&mut conn_r, &mut up_w);
        let _ = up_w.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut up_r, &mut conn_w);
    let _ = conn_w.shutdown(std::net::Shutdown::Write);
    let _ = out.join();
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::OnceLock;

    use super::*;

    static UPSTREAM: OnceLock<u16> = OnceLock::new();

    fn dial_test(target: &str) -> io::Result<TcpStream> {
        assert_eq!(target, "model.test:443");
        TcpStream::connect(("127.0.0.1", *UPSTREAM.get().unwrap()))
    }

    fn ask(sock: &Path, head: &str) -> (String, UnixStream) {
        let mut s = UnixStream::connect(sock).unwrap();
        s.write_all(head.as_bytes()).unwrap();
        let mut got = Vec::new();
        let mut byte = [0u8; 1];
        while !got.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            got.push(byte[0]);
        }
        (String::from_utf8(got).unwrap(), s)
    }

    #[test]
    fn only_a_connect_to_a_listed_host_is_spliced() {
        let up = TcpListener::bind("127.0.0.1:0").unwrap();
        UPSTREAM.set(up.local_addr().unwrap().port()).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = up.accept().unwrap();
            s.write_all(b"pong").unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("proxy.sock");
        let proxy = Proxy::start(&sock, 7, vec!["model.test".into()], dial_test).unwrap();

        let (status, mut s) = ask(&sock, "CONNECT model.test:443 HTTP/1.1\r\n\r\n");
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        let mut pong = [0u8; 4];
        s.read_exact(&mut pong).unwrap();
        assert_eq!(&pong, b"pong");

        let (status, _) = ask(&sock, "CONNECT example.com:443 HTTP/1.1\r\n\r\n");
        assert!(status.starts_with("HTTP/1.1 403"), "{status}");
        let (status, _) = ask(&sock, "GET http://model.test/ HTTP/1.1\r\n\r\n");
        assert!(status.starts_with("HTTP/1.1 405"), "{status}");

        drop(proxy);
        assert!(!sock.exists());
    }
}
