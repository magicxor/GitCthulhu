//! A tiny HTTP server on the loopback interface, for tests and the QA scenario: it stands in for the manifest host and
//! for Gravatar so the real update service and the real image fetcher can be exercised without leaving the machine
//! (the update service accepts `http` only for loopback hosts).

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// What the server answers to one request.
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Type` header value.
    pub content_type: &'static str,
    /// Body.
    pub body: Vec<u8>,
}

impl Response {
    /// A `200 OK` JSON answer.
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into().into_bytes(),
        }
    }

    /// A `200 OK` answer with image bytes.
    pub fn png(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: "image/png",
            body,
        }
    }

    /// An answer with an empty body and `status`.
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }
}

/// A running loopback server; it stops when dropped.
pub struct Loopback {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    /// Starts a server on a free port; `route` receives the request path (with query) of every request.
    pub fn start(route: impl Fn(&str) -> Response + Send + 'static) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let requests = requests.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    serve(stream, &route, &requests);
                }
            })
        };
        Ok(Self {
            addr,
            requests,
            stop,
            thread: Some(thread),
        })
    }

    /// The URL of `path` (starting with `/`) on this server.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// The paths requested so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept`.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, route: &impl Fn(&str) -> Response, log: &Mutex<Vec<String>>) {
    // A client that connects and never sends (or the wake-up connection of `Drop`) must not stall the server.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head);
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(path.clone());
    let response = route(&path);
    let header = format!(
        "HTTP/1.1 {} Status\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&response.body);
}

/// A release manifest announcing `version` with `release_url` as the link (no per-platform build).
pub fn manifest(version: &str, release_url: &str) -> String {
    format!(
        r#"{{"version": "{version}", "notes": "Release notes of {version}", "release_url": "{release_url}", "platforms": {{}}}}"#
    )
}
