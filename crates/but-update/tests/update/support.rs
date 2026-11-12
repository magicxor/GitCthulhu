//! A one-thread loopback HTTP server and cache fixtures shared by the tests.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
};

use but_db::{
    AppCacheHandle,
    cache::{CachedCheckResult, CheckUpdateStatus},
};
use chrono::{DateTime, Utc};

pub fn in_memory_cache() -> AppCacheHandle {
    AppCacheHandle::new_at_path(":memory:")
}

/// The scope used by the cache fixtures.
pub const SCOPE: &str = "0.1.0/release";

pub fn version(text: &str) -> semver::Version {
    semver::Version::parse(text).expect("test versions are valid")
}

pub fn cached_result(
    up_to_date: bool,
    latest_version: &str,
    valid_for_version: Option<&str>,
    suppressed: Option<(DateTime<Utc>, u32)>,
) -> CachedCheckResult {
    CachedCheckResult {
        checked_at: DateTime::from_timestamp(1_000_000, 0).expect("valid timestamp"),
        status: CheckUpdateStatus {
            valid_for_version: valid_for_version.map(str::to_owned),
            up_to_date,
            latest_version: latest_version.to_owned(),
            release_notes: Some("Bug fixes and improvements".to_owned()),
            url: Some("https://example.com/download".to_owned()),
            signature: Some("signature123".to_owned()),
        },
        suppressed_at: suppressed.map(|(at, _)| at),
        suppress_duration_hours: suppressed.map(|(_, hours)| hours),
    }
}

pub fn save(cache: &mut AppCacheHandle, result: &CachedCheckResult) {
    cache
        .update_check_mut()
        .expect("savepoint opens")
        .save(result)
        .expect("saving into the cache works");
}

/// Serves `responses` in order, one per connection, on a free loopback port.
pub struct Server {
    pub url: String,
    requests: mpsc::Receiver<String>,
}

impl Server {
    /// Every response is a complete HTTP/1.1 message, for example from [`ok_json`].
    pub fn start(responses: Vec<String>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port is free");
        let url = format!(
            "http://{}/latest.json",
            listener.local_addr().expect("bound address")
        );
        let (tx, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                tx.send(String::from_utf8_lossy(&request).into_owned()).ok();
                stream.write_all(response.as_bytes()).ok();
                stream.flush().ok();
            }
        });
        Server { url, requests }
    }

    /// The raw text of the next request the server received.
    pub fn next_request(&self) -> String {
        self.requests
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the server received a request")
    }
}

pub fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn ok_json(body: &str) -> String {
    response("200 OK", body)
}
