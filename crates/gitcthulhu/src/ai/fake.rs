//! A fake OpenAI-compatible provider on a loopback port, for the tests and the QA scenarios (never a real AI
//! service). It speaks just enough HTTP for chat completions, streamed or not, one thread per connection.
// The QA verbs use the streaming reply only; the tests use the rest.
#![cfg_attr(not(test), allow(dead_code))]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How the fake answers.
#[derive(Clone, Debug)]
pub enum Behavior {
    /// Stream `pieces` as chat-completion deltas, `delay` apart (non-streaming requests get them joined).
    Reply {
        /// The text pieces.
        pieces: Vec<String>,
        /// The pause before each piece.
        delay: Duration,
    },
    /// Answer every request with this status line (for example `401 Unauthorized`) and an error body.
    Status(String),
}

/// One request as the fake read it.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// The request line and headers.
    pub head: String,
    /// The body.
    pub body: String,
}

impl Recorded {
    /// The text of the last message of the chat request.
    pub fn user_prompt(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|body| {
                body["messages"]
                    .as_array()?
                    .last()?
                    .get("content")?
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_default()
    }

    /// Whether the request asked for a streamed answer.
    pub fn streamed(&self) -> bool {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .is_ok_and(|body| body["stream"].as_bool().unwrap_or(false))
    }
}

struct Shared {
    behavior: Mutex<Behavior>,
    requests: Mutex<Vec<Recorded>>,
    stop: AtomicBool,
}

/// The running fake. Dropping it stops the accept loop.
pub struct FakeProvider {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl FakeProvider {
    /// Starts a fake on a free loopback port.
    pub fn start(behavior: Behavior) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            behavior: Mutex::new(behavior),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        std::thread::Builder::new()
            .name("fake-ai-provider".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if accept_shared.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = accept_shared.clone();
                    std::thread::spawn(move || serve(stream, &shared));
                }
            })?;
        Ok(Self { addr, shared })
    }

    /// The base URL of the fake (what LM Studio's endpoint setting holds).
    pub fn endpoint(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Changes how the next requests are answered.
    pub fn set_behavior(&self, behavior: Behavior) {
        *self
            .shared
            .behavior
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = behavior;
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so that it sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(Recorded {
        head,
        body: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
    })
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    if shared.stop.load(Ordering::SeqCst) {
        return;
    }
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let chat = request
        .head
        .lines()
        .next()
        .is_some_and(|line| line.contains("/chat/completions"));
    let streamed = request.streamed();
    shared
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request);
    let behavior = shared
        .behavior
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    // A client that went away mid-stream makes a write fail: that ends the answer, nothing else to do.
    let _ = answer(&mut stream, chat, streamed, &behavior);
}

fn answer(
    stream: &mut TcpStream,
    chat: bool,
    streamed: bool,
    behavior: &Behavior,
) -> std::io::Result<()> {
    match behavior {
        Behavior::Status(status) => json(
            stream,
            status,
            r#"{"error":{"message":"the fake provider refuses"}}"#,
        ),
        _ if !chat => json(stream, "200 OK", r#"{"data":[]}"#),
        Behavior::Reply { pieces, delay } if streamed => {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )?;
            for piece in pieces {
                std::thread::sleep(*delay);
                let event = serde_json::json!({"choices": [{"delta": {"content": piece}}]});
                stream.write_all(format!("data: {event}\n\n").as_bytes())?;
                stream.flush()?;
            }
            stream.write_all(b"data: [DONE]\n\n")?;
            stream.flush()
        }
        Behavior::Reply { pieces, delay } => {
            std::thread::sleep(*delay);
            let body = serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": pieces.concat()}}]
            });
            json(stream, "200 OK", &body.to_string())
        }
    }
}

fn json(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}
