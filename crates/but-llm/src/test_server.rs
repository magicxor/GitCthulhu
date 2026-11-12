//! A throw-away HTTP server on localhost for the tests of the wire code.

use std::{future::Future, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

/// One request as the server read it.
pub(crate) struct Request {
    /// Request line and headers.
    pub head: String,
    /// The body.
    pub body: String,
}

/// Listen on an ephemeral port and run `handler` for the first connection. Returns the address and a handle that
/// yields what the handler returned.
pub(crate) async fn serve_once<F, Fut, T>(handler: F) -> (SocketAddr, tokio::task::JoinHandle<T>)
where
    F: FnOnce(Request, TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send,
    T: Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let addr = listener
        .local_addr()
        .expect("a bound socket has an address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("the client connects");
        let request = read_request(&mut stream).await;
        handler(request, stream).await
    });
    (addr, task)
}

async fn read_request(stream: &mut TcpStream) -> Request {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.expect("the request arrives");
        assert!(n > 0, "the client closed before finishing its request");
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
        let n = stream.read(&mut chunk).await.expect("the body arrives");
        assert!(n > 0, "the client closed before finishing its body");
        buf.extend_from_slice(&chunk[..n]);
    }
    Request {
        head,
        body: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
    }
}

/// Answer with a complete response and close.
pub(crate) async fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("the client reads the response");
    stream.shutdown().await.ok();
}

/// Start an event-stream response whose body runs until the connection closes.
pub(crate) async fn start_event_stream(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .await
        .expect("the client reads the head");
}

/// Send raw event-stream text.
pub(crate) async fn send(stream: &mut TcpStream, text: &str) {
    stream
        .write_all(text.as_bytes())
        .await
        .expect("the client reads the events");
    stream.flush().await.expect("flush");
}
