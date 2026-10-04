use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use std::time::Duration;

pub const HEADER_TIMEOUT: Duration = Duration::from_secs(120);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(180);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
pub const BODY_LIMIT: usize = 32 * 1024 * 1024;
pub const EVENT_LIMIT: usize = 1024 * 1024;
pub const ERROR_LIMIT: usize = 16 * 1024;

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
pub struct Frame {
    pub raw: Vec<u8>,
    pub data: String,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        Ok(self
            .push_frames(bytes)?
            .into_iter()
            .filter_map(|frame| (!frame.data.is_empty()).then_some(frame.data))
            .collect())
    }
    pub fn push_frames(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        let mut events = Vec::new();
        // Only a newline can complete a frame. Copy each line in bulk rather
        // than checking three delimiters after every byte of a long JSON line.
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if line.len() > EVENT_LIMIT.saturating_sub(self.pending.len()) {
                bail!("upstream SSE event exceeds 1 MiB");
            }
            self.pending.extend_from_slice(line);
            let separator = if self.pending.ends_with(b"\r\n\r\n") {
                4
            } else if self.pending.ends_with(b"\n\r\n") {
                3
            } else if self.pending.ends_with(b"\n\n") {
                2
            } else {
                continue;
            };
            let frame = std::str::from_utf8(&self.pending[..self.pending.len() - separator])
                .context("invalid UTF-8 in upstream SSE")?;
            let data = frame
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|s| s.strip_prefix(' ').unwrap_or(s))
                })
                .collect::<Vec<_>>()
                .join("\n");
            events.push(Frame {
                raw: std::mem::take(&mut self.pending),
                data,
            });
        }
        Ok(events)
    }
}

pub async fn read_body(
    response: reqwest::Response,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = limit.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            if truncate {
                bytes.extend_from_slice(&chunk[..remaining]);
                return Ok(bytes);
            }
            bail!("upstream response exceeds {} bytes", limit);
        }
        bytes.extend_from_slice(&chunk);
        if truncate && bytes.len() == limit {
            break;
        }
    }
    Ok(bytes)
}

/// Only error frames are rewritten; normal content and comment frames retain
/// their exact bytes even when network chunks split a secret or UTF-8 scalar.
pub(super) fn redact_stream(
    response: reqwest::Response,
    credential: crate::config::Credential,
    local_token: String,
) -> reqwest::Response {
    let mut builder = axum::http::Response::builder()
        .status(response.status())
        .version(response.version());
    let mut headers = response.headers().clone();
    headers.remove(axum::http::header::CONTENT_LENGTH);
    *builder.headers_mut().expect("response headers") = headers;
    let mut upstream = response.bytes_stream();
    let output = async_stream::stream! {
        let mut decoder = Decoder::default();
        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk { Ok(chunk) => chunk, Err(error) => { yield Err(std::io::Error::other(error)); return; } };
            let frames = match decoder.push_frames(&chunk) { Ok(frames) => frames, Err(error) => { yield Err(std::io::Error::other(error.to_string())); return; } };
            for frame in frames {
                let mut raw = frame.raw;
                if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&frame.data)
                    && (value.get("error").is_some_and(|v| !v.is_null()) || matches!(value["type"].as_str(), Some("error" | "response.failed"))) {
                    crate::diagnostics::redact(&mut value, &credential, &local_token);
                    let event = value["type"].as_str().unwrap_or("error");
                    raw = format!("event: {event}\ndata: {value}\n\n").into_bytes();
                }
                yield Ok::<_, std::io::Error>(bytes::Bytes::from(raw));
            }
        }
    };
    builder
        .body(reqwest::Body::wrap_stream(output))
        .expect("stream response")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_split_at_every_byte_and_mixed_delimiters() {
        let input = "data: 中文😀\r\n\r\n: heartbeat\n\ndata: one\ndata: two\n\r\n";
        for split in 0..=input.len() {
            let mut decoder = Decoder::default();
            let mut out = decoder.push(&input.as_bytes()[..split]).unwrap();
            out.extend(decoder.push(&input.as_bytes()[split..]).unwrap());
            assert_eq!(out, vec!["中文😀", "one\ntwo"]);
        }
    }
    #[test]
    fn reject_unbounded_or_invalid_events() {
        assert!(
            Decoder::default()
                .push(&vec![b'x'; EVENT_LIMIT + 1])
                .is_err()
        );
        assert!(Decoder::default().push(b"data: \xff\n\n").is_err());
    }
    #[test]
    fn event_limit_applies_per_frame_and_before_buffer_growth() {
        let mut frame = b"data: ".to_vec();
        frame.extend(vec![b'x'; EVENT_LIMIT - frame.len() - 2]);
        frame.extend_from_slice(b"\n\n");
        let combined = [frame.as_slice(), frame.as_slice()].concat();
        let frames = Decoder::default().push_frames(&combined).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].raw, frame);
        let mut decoder = Decoder::default();
        decoder.push(b"data: ").unwrap();
        let before = decoder.pending.len();
        assert!(decoder.push(&vec![b'x'; EVENT_LIMIT]).is_err());
        assert_eq!(decoder.pending.len(), before);
    }
    #[test]
    #[ignore = "manual local SSE decoder benchmark"]
    fn sse_decoder_benchmark() {
        fn original(input: &[u8], chunk_size: usize) -> Vec<String> {
            let mut pending = Vec::new();
            let mut events = Vec::new();
            for chunk in input.chunks(chunk_size) {
                for byte in chunk {
                    pending.push(*byte);
                    assert!(pending.len() <= EVENT_LIMIT);
                    let separator = if pending.ends_with(b"\r\n\r\n") {
                        4
                    } else if pending.ends_with(b"\n\r\n") {
                        3
                    } else if pending.ends_with(b"\n\n") {
                        2
                    } else {
                        continue;
                    };
                    let frame = std::str::from_utf8(&pending[..pending.len() - separator]).unwrap();
                    let data = frame
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("data:")
                                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _raw = std::mem::take(&mut pending);
                    if !data.is_empty() {
                        events.push(data);
                    }
                }
            }
            events
        }
        fn bulk(input: &[u8], chunk_size: usize) -> Vec<String> {
            let mut decoder = Decoder::default();
            input
                .chunks(chunk_size)
                .flat_map(|chunk| decoder.push(chunk).unwrap())
                .collect()
        }
        fn median(mut values: Vec<u128>) -> u128 {
            values.sort();
            values[values.len() / 2]
        }
        for (label, text) in [
            (
                "long-event",
                format!("data: {}\n\n", "中文abcdef".repeat(20_000)),
            ),
            (
                "small-events",
                "data: {\"text\":\"中文\"}\r\n\r\n".repeat(10_000),
            ),
        ] {
            let input = text.as_bytes();
            assert_eq!(original(input, 4096), bulk(input, 4096));
            let mut before = Vec::new();
            let mut after = Vec::new();
            for _ in 0..7 {
                let start = std::time::Instant::now();
                let old = original(input, 4096);
                before.push(start.elapsed().as_micros());
                let start = std::time::Instant::now();
                let new = bulk(input, 4096);
                after.push(start.elapsed().as_micros());
                assert_eq!(old, new);
            }
            let (before, after) = (median(before), median(after));
            eprintln!(
                "{label}: bytewise p50={before} us; bulk p50={after} us; speedup={:.1}x",
                before as f64 / after.max(1) as f64
            );
        }
    }
}

#[cfg(test)]
mod diagnostic_stream_tests {
    use super::*;
    #[tokio::test]
    async fn streaming_redacts_split_error_frames_and_preserves_success_bytes() {
        let normal = "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"upstream-secret 中文\"}}\r\n\r\n: heartbeat\n\n";
        let error = "data: {\"type\":\"error\",\"error\":{\"message\":\"upstream-secret local-secret\"}}\n\n";
        let all = format!("{normal}{error}");
        let chunks = all
            .bytes()
            .map(|b| Ok::<_, std::io::Error>(bytes::Bytes::from(vec![b])))
            .collect::<Vec<_>>();
        let response = reqwest::Response::from(axum::http::Response::new(
            reqwest::Body::wrap_stream(futures_util::stream::iter(chunks)),
        ));
        let clean = redact_stream(
            response,
            crate::config::Credential::Bearer {
                value: "upstream-secret".into(),
            },
            "local-secret".into(),
        )
        .bytes()
        .await
        .unwrap();
        assert!(clean.starts_with(normal.as_bytes()));
        let tail = std::str::from_utf8(&clean[normal.len()..]).unwrap();
        assert!(!tail.contains("secret"));
        assert!(tail.contains("[redacted]"));
    }
}
