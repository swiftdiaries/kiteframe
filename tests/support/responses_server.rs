use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

pub struct ResponsesServer {
    base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
}

impl ResponsesServer {
    pub async fn start(response: serde_json::Value) -> Self {
        Self::start_sequence(vec![response]).await
    }

    #[allow(dead_code)] // Used by the separate responses-conformance test binary.
    pub async fn start_with_status(status: u16, response: serde_json::Value) -> Self {
        Self::start_sequence_with_status(status, vec![response]).await
    }

    pub async fn start_sequence(responses: Vec<serde_json::Value>) -> Self {
        Self::start_sequence_with_status(200, responses).await
    }

    pub async fn start_sequence_with_status(
        status: u16,
        responses: Vec<serde_json::Value>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind response test server");
        let address = listener.local_addr().expect("read local address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.expect("accept test request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).await.expect("read test request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers =
                            std::str::from_utf8(&request[..header_end]).expect("utf8 headers");
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length: ")
                                    .or_else(|| line.strip_prefix("Content-Length: "))
                            })
                            .and_then(|value| value.parse::<usize>().ok())
                            .expect("content length");
                        if request.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("header end");
                let headers = std::str::from_utf8(&request[..header_end]).expect("utf8 headers");
                let request_line = headers.lines().next().expect("request line");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().expect("method").to_owned();
                let path = parts.next().expect("path").to_owned();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .or_else(|| line.strip_prefix("Content-Length: "))
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .expect("content length");
                let body = serde_json::from_slice(
                    &request[header_end + 4..header_end + 4 + content_length],
                )
                .expect("JSON request body");
                captured
                    .lock()
                    .await
                    .push(CapturedRequest { method, path, body });

                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(reply.as_bytes())
                    .await
                    .expect("write test response");
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            requests,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn one_request(&self) -> CapturedRequest {
        for _ in 0..100 {
            if let Some(request) = self.requests.lock().await.pop() {
                return request;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("expected one request")
    }
}
