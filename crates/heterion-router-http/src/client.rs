//! Explicit HTTP client.
//!
//! Timeouts are split by *what* they bound, because one wall over the whole
//! response is the wrong shape for a gateway that streams: `reqwest`'s
//! `timeout` is a total deadline that keeps counting while the body is still
//! arriving, so a single 600s value silently truncated every generation that
//! streamed longer than ten minutes — mid-flight, reported as an upstream
//! error. [`HttpTimeouts`] separates the three phases instead:
//!
//! * `connect` — TCP/TLS handshake.
//! * `response` — how long the upstream may take to start answering at all.
//! * `idle` — how long the body may stay *silent*; it resets on every chunk,
//!   so a long-but-healthy stream is never killed while a dead one is still
//!   bounded.
//!
//! Caveat for reqwest 0.12: `read_timeout` also covers the pre-response phase,
//! so `response` only ever bites when it is *shorter* than `idle`.

use serde_json::Value;
use std::time::Duration;
use thiserror::Error;

/// HTTP failures.
#[derive(Debug, Error)]
pub enum HttpError {
    /// Transport / protocol failure.
    #[error("http transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The upstream never started responding inside its budget.
    #[error("upstream did not start responding within {0:?}")]
    NoResponse(Duration),

    /// Upstream answered with a non-success status.
    #[error("upstream status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Truncated response body for diagnostics.
        body: String,
    },
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, HttpError>;

/// The three independent timeout budgets of a [`HttpClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpTimeouts {
    /// Budget for the TCP/TLS connect phase.
    pub connect: Duration,
    /// Budget for "the upstream starts responding" (request sent → response
    /// headers in hand).
    pub response: Duration,
    /// Budget for the silence *between* body chunks. Reset by every chunk, so
    /// it measures idleness, never total stream length.
    pub idle: Duration,
}

impl HttpTimeouts {
    /// One budget for all three phases — what `HttpClient::new(secs)` means.
    pub fn uniform(each: Duration) -> Self {
        Self {
            connect: each,
            response: each,
            idle: each,
        }
    }
}

/// Shared async HTTP client. Cheap to clone; holds connection pools.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    timeouts: HttpTimeouts,
}

impl HttpClient {
    /// Build a client whose connect, response and idle budgets are all
    /// `timeout_secs`. The budget is *not* a total deadline: a stream may run
    /// for as long as it keeps producing chunks.
    pub fn new(timeout_secs: u64) -> Result<Self> {
        Self::with_timeouts(HttpTimeouts::uniform(Duration::from_secs(timeout_secs)))
    }

    /// Build a client with the three budgets split apart.
    pub fn with_timeouts(timeouts: HttpTimeouts) -> Result<Self> {
        let inner = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .read_timeout(timeouts.idle)
            .build()?;
        Ok(Self { inner, timeouts })
    }

    /// The budgets this client enforces.
    pub fn timeouts(&self) -> HttpTimeouts {
        self.timeouts
    }

    /// Send a request and bound how long the upstream may take to *start*
    /// answering. `RequestBuilder::send` resolves once the response headers
    /// are in, so this deadline covers connect + request + waiting for the
    /// first byte of the response and nothing after it; the body is then
    /// streamed under the idle timeout.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        match tokio::time::timeout(self.timeouts.response, request.send()).await {
            Ok(result) => result.map_err(HttpError::from),
            Err(_elapsed) => Err(HttpError::NoResponse(self.timeouts.response)),
        }
    }

    /// POST a JSON body, returning the response for the caller to stream or
    /// buffer. Bearer auth is attached when `bearer` is `Some`.
    pub async fn post_json(
        &self,
        url: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Result<reqwest::Response> {
        self.post_json_with_headers(url, bearer, &[], body).await
    }

    /// POST form-encoded data, returning the response for the caller.
    pub async fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<reqwest::Response> {
        let response = self.send(self.inner.post(url).form(fields)).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(2000)
                .collect();
            return Err(HttpError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(response)
    }

    /// POST a JSON body with extra headers (provider session headers, etc.).
    pub async fn post_json_with_headers(
        &self,
        url: &str,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<reqwest::Response> {
        let mut request = self.inner.post(url);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self.send(request.json(body)).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(2000)
                .collect();
            return Err(HttpError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string_contains, header, method, path},
    };

    /// Bare HTTP/1.1 origin that streams `chunks` chunked bodies `gap` apart,
    /// then ends the response. `stall` keeps the connection open and silent
    /// after the first chunk. Hand-rolled because the point is *when* bytes
    /// arrive, which a request/response mock server cannot express.
    async fn streaming_origin(chunks: usize, gap: Duration, stall: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 2048];
            let _ = socket.read(&mut head).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      transfer-encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            for index in 0..chunks {
                let frame = format!("data: {index}\n\n");
                socket
                    .write_all(format!("{:x}\r\n", frame.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(frame.as_bytes()).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
                socket.flush().await.unwrap();
                if stall {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                tokio::time::sleep(gap).await;
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        });
        format!("http://{address}")
    }

    /// Read a chunked body to the end, returning `(frames, transport error)`.
    async fn drain(mut response: reqwest::Response) -> (usize, Option<reqwest::Error>) {
        let mut frames = 0usize;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    frames += std::str::from_utf8(&chunk)
                        .unwrap_or_default()
                        .matches("data: ")
                        .count()
                }
                Ok(None) => return (frames, None),
                Err(error) => return (frames, Some(error)),
            }
        }
    }

    /// RED (defect 2): a healthy stream that keeps trickling must NOT be cut
    /// off by a deadline measured over the whole body. 40 chunks 40ms apart is
    /// ~1.6s of healthy SSE — longer than the 1s budget this client is built
    /// with, and every individual gap is 40ms.
    #[tokio::test]
    async fn a_trickling_stream_outlives_the_old_total_deadline() {
        let url = streaming_origin(40, Duration::from_millis(40), false).await;
        let client = HttpClient::new(1).unwrap();
        let response = client
            .post_json(&url, None, &serde_json::json!({}))
            .await
            .unwrap();
        let (frames, error) = drain(response).await;
        assert!(
            error.is_none(),
            "a healthy 1.6s stream was cut off: {error:?}"
        );
        assert_eq!(frames, 40, "every frame must survive");
    }

    /// The other half of the contract: an upstream that stops sending is
    /// still bounded by a timeout instead of hanging the request forever.
    #[tokio::test]
    async fn a_stream_that_stops_producing_chunks_is_cut_off() {
        let url = streaming_origin(1, Duration::from_millis(0), true).await;
        let client = HttpClient::new(1).unwrap();
        let response = client
            .post_json(&url, None, &serde_json::json!({}))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let (_, error) = drain(response).await;
        let elapsed = started.elapsed();

        let error = error.expect("a stalled upstream must not stream forever");
        assert!(error.is_timeout(), "expected a timeout, got {error:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "took {elapsed:?} to give up"
        );
    }

    /// Same two guarantees at millisecond scale, so they are cheap to run and
    /// provable: 400ms of healthy trickling under a 100ms idle budget survives
    /// (the gaps are 40ms), while the same client gives up on a stalled
    /// upstream within the idle budget rather than the response budget.
    #[tokio::test]
    async fn idle_budget_is_between_chunks_not_over_the_whole_body() {
        let timeouts = HttpTimeouts {
            connect: Duration::from_secs(2),
            response: Duration::from_secs(10),
            idle: Duration::from_millis(100),
        };

        // 10 chunks, 40ms apart: 400ms of healthy streaming, 10x the idle
        // budget in total.
        let healthy = streaming_origin(10, Duration::from_millis(40), false).await;
        let client = HttpClient::with_timeouts(timeouts).unwrap();
        let response = client
            .post_json(&healthy, None, &serde_json::json!({}))
            .await
            .unwrap();
        let (frames, error) = drain(response).await;
        assert!(error.is_none(), "healthy stream was cut off: {error:?}");
        assert_eq!(frames, 10, "every frame must survive");

        // Same client, same idle budget: a stalled upstream dies at the idle
        // budget, not at the 10s response budget.
        let stalled = streaming_origin(1, Duration::from_millis(0), true).await;
        let response = client
            .post_json(&stalled, None, &serde_json::json!({}))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let (frames, error) = drain(response).await;
        let elapsed = started.elapsed();
        assert_eq!(frames, 1, "the first frame was delivered");
        let error = error.expect("a stalled upstream must not stream forever");
        assert!(error.is_timeout(), "expected a timeout, got {error:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "gave up only after {elapsed:?}: the idle budget did not apply"
        );
    }

    /// An upstream that never answers at all fails as `NoResponse`, not as a
    /// transport error the caller has to guess about.
    #[tokio::test]
    async fn an_upstream_that_never_starts_responding_is_a_no_response_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Accept and then say nothing at all.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let client = HttpClient::with_timeouts(HttpTimeouts {
            connect: Duration::from_secs(2),
            response: Duration::from_millis(100),
            idle: Duration::from_secs(10),
        })
        .unwrap();
        let error = client
            .post_json(&format!("http://{address}/x"), None, &serde_json::json!({}))
            .await
            .expect_err("a silent upstream must not hang the request");
        match error {
            HttpError::NoResponse(budget) => assert_eq!(budget, Duration::from_millis(100)),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn post_json_sends_bearer_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(serde_json::json!({"model": "m"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let client = HttpClient::new(10).unwrap();
        let response = client
            .post_json(
                &format!("{}/v1/responses", server.uri()),
                Some("tok"),
                &serde_json::json!({"model": "m"}),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        server.verify().await;

        // The test is named for what goes on the wire, so assert it rather
        // than relying on the mock matcher alone.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].headers["authorization"], "Bearer tok");
        assert_eq!(
            sent[0].body_json::<serde_json::Value>().unwrap(),
            serde_json::json!({"model": "m"}),
            "the JSON body must be sent verbatim"
        );
    }

    #[tokio::test]
    async fn post_form_sends_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = HttpClient::new(10).unwrap();
        let response = client
            .post_form(
                &format!("{}/token", server.uri()),
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", "refresh"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        server.verify().await;

        // Named for the fields, so check the fields actually arrived.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].headers["content-type"],
            "application/x-www-form-urlencoded"
        );
        let body = String::from_utf8(sent[0].body.clone()).unwrap();
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(body.contains("refresh_token=refresh"), "{body}");
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .mount(&server)
            .await;

        let client = HttpClient::new(10).unwrap();
        let error = client
            .post_json(&format!("{}/x", server.uri()), None, &serde_json::json!({}))
            .await
            .unwrap_err();
        match error {
            HttpError::Status { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("slow down"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
