//! Adapter: the relay inside a worker sandbox (`idlemax sandbox-relay`). The sandbox has only
//! loopback, so the CLI's proxy variables point here, and every connection is forwarded to the
//! agent's proxy socket, which decides what it may reach. The relay holds no secret.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Listens on `127.0.0.1:<port>`, writes `ready`, then forwards each connection until killed.
pub fn run(socket: &Path, port: u16, ready: &Path) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    std::fs::write(ready, b"")?;
    serve(listener, socket)
}

fn serve(listener: TcpListener, socket: &Path) -> io::Result<()> {
    for conn in listener.incoming() {
        let Ok(tcp) = conn else { continue };
        let socket = socket.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(unix) = UnixStream::connect(&socket) {
                splice_tcp_unix(tcp, unix);
            }
        });
    }
    Ok(())
}

/// Copies both ways until either side closes.
fn splice_tcp_unix(tcp: TcpStream, unix: UnixStream) {
    let (Ok(mut tcp_r), Ok(mut unix_w)) = (tcp.try_clone(), unix.try_clone()) else {
        return;
    };
    let (mut unix_r, mut tcp_w) = (unix, tcp);
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut tcp_r, &mut unix_w);
        let _ = unix_w.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut unix_r, &mut tcp_w);
    let _ = tcp_w.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn bytes_go_both_ways_between_a_tcp_client_and_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("p.sock");
        let upstream = UnixListener::bind(&sock).unwrap();
        let echo = std::thread::spawn(move || {
            let (mut s, _) = upstream.accept().unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).unwrap();
            s.write_all(&buf.map(|b| b.to_ascii_uppercase())).unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sock2 = sock.clone();
        std::thread::spawn(move || serve(listener, &sock2));
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(b"hello").unwrap();
        let mut back = [0u8; 5];
        client.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"HELLO");
        echo.join().unwrap();
    }
}
