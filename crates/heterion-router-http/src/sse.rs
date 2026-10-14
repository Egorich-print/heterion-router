//! Minimal SSE event decoder.
//!
//! Splits a byte/text stream into `(event, data)` frames. Handles split
//! chunks, `\r\n` line endings, multi-line `data:` (joined with `\n`) and
//! comment/keepalive lines (`: ...`), which are dropped. A `data: [DONE]`
//! sentinel is surfaced as a normal event so the caller can terminate.
//!
//! Both resources an upstream controls are bounded: one un-terminated event
//! may buffer at most [`MAX_SSE_EVENT_BYTES`], and the buffer is trimmed once
//! per chunk rather than drained per line.

use std::borrow::Cow;

/// One decoded SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` name, if the frame carried one.
    pub event: Option<String>,
    /// The concatenated `data:` payload.
    pub data: String,
}

/// Hard cap on the bytes a single un-terminated SSE event may buffer
/// (1 MiB). A stream that never sends a newline must fail loudly instead of
/// growing: the gateway holds one decoder per in-flight upstream response, so
/// an unbounded buffer is a memory-exhaustion lever for any upstream.
/// The cap covers what the decoder holds between pushes, so a single larger
/// chunk may transiently add its own length on top of it.
pub const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// Failure frame for a frame that outgrew [`MAX_SSE_EVENT_BYTES`]. Same
/// `{"error":{"message":...}}` shape the gateway's `sanitized_error_payload`
/// emits, so every client-facing surface fails closed on it. Carries no
/// upstream text: only the decoder's own reason for giving up.
const OVERFLOW_ERROR_DATA: &str =
    r#"{"error":{"message":"upstream sse event exceeded the decoder limit"}}"#;

/// Incremental decoder: feed text or byte chunks, drain complete frames.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    frame: String,
    overflowed: bool,
}

impl SseDecoder {
    /// Empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes held for the event currently being assembled.
    pub fn buffered(&self) -> usize {
        self.buffer.len() + self.frame.len()
    }

    /// Push text, returning every frame completed by this chunk.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Push bytes, preserving UTF-8 sequences split across network chunks.
    ///
    /// One scan per chunk finds the line ends and a single `truncate` drops
    /// what was consumed, so a large stream costs time linear in its length.
    /// A single un-terminated event is capped at [`MAX_SSE_EVENT_BYTES`]:
    /// on overflow the decoder emits one `event: error` frame carrying
    /// [`OVERFLOW_ERROR_DATA`], drops everything it buffered and ignores the
    /// rest of the stream. Raw upstream text is never echoed back.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if self.overflowed {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut consumed = 0;
        while let Some((line_end, delimiter_len)) = next_line(&self.buffer[consumed..]) {
            let bytes = &self.buffer[consumed..consumed + line_end];
            let line: Cow<'_, str> = match std::str::from_utf8(bytes) {
                Ok(line) => Cow::Borrowed(line),
                Err(_) => String::from_utf8_lossy(bytes),
            };
            consumed += line_end + delimiter_len;
            if line.is_empty() {
                if let Some(event) = parse_frame(&self.frame) {
                    events.push(event);
                }
                self.frame.clear();
            } else {
                self.frame.push_str(&line);
                self.frame.push('\n');
            }
        }
        if consumed > 0 {
            let remaining = self.buffer.len() - consumed;
            self.buffer.copy_within(consumed.., 0);
            self.buffer.truncate(remaining);
        }
        if self.buffered() > MAX_SSE_EVENT_BYTES {
            self.buffer = Vec::new();
            self.frame = String::new();
            self.overflowed = true;
            events.push(SseEvent {
                event: Some("error".to_string()),
                data: OVERFLOW_ERROR_DATA.to_string(),
            });
        }
        events
    }
}

fn next_line(buffer: &[u8]) -> Option<(usize, usize)> {
    for (index, byte) in buffer.iter().enumerate() {
        match *byte {
            b'\n' => return Some((index, 1)),
            b'\r' if index + 1 >= buffer.len() => return None,
            b'\r' => return Some((index, if buffer[index + 1] == b'\n' { 2 } else { 1 })),
            _ => {}
        }
    }
    None
}

fn parse_frame(frame: &str) -> Option<SseEvent> {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(name) = line.strip_prefix("event:") {
            event = Some(name.trim().to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            // Per SSE, one leading space after the colon is stripped.
            data_lines.push(payload.strip_prefix(' ').unwrap_or(payload));
        }
    }
    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_frames() {
        let mut decoder = SseDecoder::new();
        // Split mid-frame, mid-line and mid-`\n\n`.
        let first = decoder.push("event: response.output_text.del");
        assert!(first.is_empty());
        let second = decoder.push("ta\ndata: {\"delta\": \"Hi\"}\n");
        assert!(second.is_empty());
        let third = decoder.push("\ndata: {\"delta\": \"!\"}\n\n");
        assert_eq!(third.len(), 2);
        assert_eq!(
            third[0].event.as_deref(),
            Some("response.output_text.delta")
        );
        assert_eq!(third[0].data, "{\"delta\": \"Hi\"}");
        assert_eq!(third[1].event, None);
        assert_eq!(third[1].data, "{\"delta\": \"!\"}");
    }

    #[test]
    fn drops_comments_and_handles_crlf() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(": keepalive\r\nevent: ping\r\ndata: hi\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("ping"));
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn joins_multiline_data() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push("data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn preserves_utf8_split_across_byte_chunks() {
        let input = b"data: {\"text\":\"\xC3\xA9\xF0\x9F\x98\x80\"}\n\n";
        for split in 1..input.len() {
            let mut decoder = SseDecoder::new();
            let mut events = decoder.push_bytes(&input[..split]);
            events.extend(decoder.push_bytes(&input[split..]));
            assert_eq!(events.len(), 1, "split at {split}");
            assert_eq!(events[0].data, "{\"text\":\"é😀\"}");
        }
    }

    #[test]
    fn preserves_crlf_split_across_byte_chunks() {
        let input = b"data: hi\r\n\r\n";
        for split in 1..input.len() {
            let mut decoder = SseDecoder::new();
            let mut events = decoder.push_bytes(&input[..split]);
            events.extend(decoder.push_bytes(&input[split..]));
            assert_eq!(events.len(), 1, "split at {split}");
            assert_eq!(events[0].data, "hi");
        }
    }

    /// Decoder as it was written before the bound/linearity fix: drains the
    /// front of the buffer per line. Kept here so the fast decoder can be
    /// pinned to byte-for-byte identical output.
    struct NaiveDecoder {
        buffer: Vec<u8>,
        frame: String,
    }

    impl NaiveDecoder {
        fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
            self.buffer.extend_from_slice(chunk);
            let mut events = Vec::new();
            while let Some((line_end, delimiter_len)) = next_line(&self.buffer) {
                let line_bytes: Vec<u8> = self.buffer.drain(..line_end + delimiter_len).collect();
                let line = String::from_utf8_lossy(&line_bytes[..line_end]);
                if line.is_empty() {
                    if let Some(event) = parse_frame(&self.frame) {
                        events.push(event);
                    }
                    self.frame.clear();
                } else {
                    self.frame.push_str(&line);
                    self.frame.push('\n');
                }
            }
            events
        }
    }

    fn feed_both(stream: &[u8], chunk_size: usize) -> (Vec<SseEvent>, Vec<SseEvent>) {
        let mut fast = SseDecoder::new();
        let mut naive = NaiveDecoder {
            buffer: Vec::new(),
            frame: String::new(),
        };
        let mut fast_events = Vec::new();
        let mut naive_events = Vec::new();
        for chunk in stream.chunks(chunk_size) {
            fast_events.extend(fast.push_bytes(chunk));
            naive_events.extend(naive.push(chunk));
        }
        (fast_events, naive_events)
    }

    fn error_events(events: &[SseEvent]) -> Vec<&SseEvent> {
        events
            .iter()
            .filter(|event| event.event.as_deref() == Some("error"))
            .collect()
    }

    #[test]
    fn unterminated_event_errors_instead_of_buffering_forever() {
        let mut decoder = SseDecoder::new();
        let chunk = vec![b'x'; 64 * 1024];
        let started = std::time::Instant::now();
        let mut errors: Vec<SseEvent> = Vec::new();
        let mut peak = 0;
        // 8 MiB of upstream text with no newline at all.
        for _ in 0..128 {
            let pushed = decoder.push_bytes(&chunk);
            errors.extend(error_events(&pushed).into_iter().cloned());
            peak = peak.max(decoder.buffered());
        }
        let elapsed = started.elapsed();

        assert_eq!(
            errors.len(),
            1,
            "a stream with no newline must fail exactly once, got {} error events",
            errors.len()
        );
        let payload: serde_json::Value =
            serde_json::from_str(&errors[0].data).expect("error frame carries JSON");
        let message = payload["error"]["message"]
            .as_str()
            .expect("error frame carries the sanitized error/message shape");
        assert!(
            !message.contains(&"x".repeat(16)),
            "the decoder must not echo raw upstream text: {message}"
        );
        assert!(
            peak <= MAX_SSE_EVENT_BYTES + chunk.len(),
            "buffered {peak} bytes without a newline"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "8 MiB without a newline took {elapsed:?}"
        );
    }

    #[test]
    fn large_multi_event_stream_matches_the_naive_decoder() {
        let mut stream = Vec::new();
        for index in 0..20_000 {
            stream.extend_from_slice(format!("event: tick.{index}\n").as_bytes());
            stream.extend_from_slice(format!("data: {{\"i\":{index}}}\n\n").as_bytes());
        }
        for chunk_size in [1, 7, 64 * 1024, 3_000_000] {
            let (fast, naive) = feed_both(&stream, chunk_size);
            assert_eq!(fast, naive, "chunk size {chunk_size}");
            assert_eq!(fast.len(), 20_000, "chunk size {chunk_size}");
            assert!(error_events(&fast).is_empty());
        }
    }

    #[test]
    fn short_events_match_the_naive_decoder() {
        let stream = b": ping\r\ndata: one\ndata: two\nevent: done\ndata: [DONE]\n\n\
                    event: second\r\ndata: {\"a\":1}\n\ndata: trailing";
        for chunk_size in 1..=stream.len() {
            let (fast, naive) = feed_both(stream, chunk_size);
            assert_eq!(fast, naive, "chunk size {chunk_size}");
        }
        let mut decoder = SseDecoder::new();
        let events = decoder.push("data: hello\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "hello".to_string()
            }]
        );
    }
}
