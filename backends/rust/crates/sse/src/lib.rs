//! Reads server-sent events across arbitrary chunk and CRLF boundaries. Content is never logged.
//!
//! A hostile upstream must not be able to exhaust memory, so one event may not exceed [`MAX_FRAME_BYTES`], and
//! that includes a single line that never ends: bytes are checked as they arrive and the read is abandoned the
//! moment the budget is passed, before the overflowing bytes are copied.

use std::collections::VecDeque;
use std::fmt;

use futures_util::{Stream, StreamExt};

/// Caps one event so a hostile upstream cannot exhaust memory.
pub const MAX_FRAME_BYTES: usize = 2_000_000;

/// One event: its name (`message` when absent) and its joined data lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub event: String,
    pub data: String,
}

/// An event passed [`MAX_FRAME_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTooLarge;

impl fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sse: frame too large")
    }
}

impl std::error::Error for FrameTooLarge {}

/// Why [`frames`] stopped: the event was too large, or the underlying body failed.
#[derive(Debug)]
pub enum Error<E> {
    FrameTooLarge,
    Read(E),
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge => f.write_str("sse: frame too large"),
            Self::Read(e) => write!(f, "sse: read failed: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Error<E> {}

/// An incremental parser: feed it bytes in any chunking, collect complete frames. A trailing event with no
/// terminating blank line is never produced, as in the TypeScript reference.
#[derive(Debug)]
pub struct Parser {
    limit: usize,
    /// The unfinished line.
    line: Vec<u8>,
    /// Bytes of the finished lines of the current event, newlines included.
    size: usize,
    event: String,
    data: Vec<String>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self::with_limit(MAX_FRAME_BYTES)
    }

    pub fn with_limit(limit: usize) -> Self {
        Self { limit, line: Vec::new(), size: 0, event: "message".to_string(), data: Vec::new() }
    }

    /// Consumes `chunk`, appending every frame it completes to `out`. On [`FrameTooLarge`] the frames completed
    /// before the oversized line are still in `out`, and the parser must not be used again.
    pub fn push(&mut self, mut chunk: &[u8], out: &mut VecDeque<Frame>) -> Result<(), FrameTooLarge> {
        while !chunk.is_empty() {
            let (piece, terminated) = match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (&chunk[..=i], true),
                None => (chunk, false),
            };
            if self.size + self.line.len() + piece.len() > self.limit {
                return Err(FrameTooLarge);
            }
            self.line.extend_from_slice(piece);
            chunk = &chunk[piece.len()..];
            if terminated {
                let line = std::mem::take(&mut self.line);
                self.size += line.len();
                self.finish_line(&line, out);
            }
        }
        Ok(())
    }

    fn finish_line(&mut self, line: &[u8], out: &mut VecDeque<Frame>) {
        let text = String::from_utf8_lossy(line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !self.data.is_empty() {
                out.push_back(Frame { event: self.event.clone(), data: self.data.join("\n") });
            }
            self.event = "message".to_string();
            self.size = 0;
            self.data.clear();
        } else if let Some(name) = trimmed.strip_prefix("event:") {
            self.event = name.trim().to_string();
        } else if let Some(value) = trimmed.strip_prefix("data:") {
            self.data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
    }
}

/// Turns a body of byte chunks into frames. The stream ends after the first error. Dropping it drops the body.
pub fn frames<S, B, E>(body: S) -> impl Stream<Item = Result<Frame, Error<E>>>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    async_stream::stream! {
        let mut body = std::pin::pin!(body);
        let mut parser = Parser::new();
        let mut ready = VecDeque::new();
        loop {
            while let Some(frame) = ready.pop_front() {
                yield Ok(frame);
            }
            match body.next().await {
                None => return,
                Some(Err(e)) => {
                    yield Err(Error::Read(e));
                    return;
                }
                Some(Ok(chunk)) => {
                    if parser.push(chunk.as_ref(), &mut ready).is_err() {
                        while let Some(frame) = ready.pop_front() {
                            yield Ok(frame);
                        }
                        yield Err(Error::FrameTooLarge);
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures_util::stream;

    use super::*;

    fn chunked(input: &str, size: usize) -> impl Stream<Item = Result<Vec<u8>, Infallible>> {
        let chunks: Vec<_> = input.as_bytes().chunks(size).map(|c| Ok(c.to_vec())).collect();
        stream::iter(chunks)
    }

    async fn collect(input: &str, size: usize) -> Vec<Frame> {
        let mut out = Vec::new();
        let mut s = std::pin::pin!(frames(chunked(input, size)));
        while let Some(f) = s.next().await {
            out.push(f.expect("no error"));
        }
        out
    }

    fn f(event: &str, data: &str) -> Frame {
        Frame { event: event.to_string(), data: data.to_string() }
    }

    #[tokio::test]
    async fn frames_across_chunk_and_crlf_boundaries() {
        let input = "event: a\r\ndata: one\r\n\r\ndata: two\ndata: lines\n\n: comment\n\nevent: last\ndata:  spaced\n\n";
        let want = vec![f("a", "one"), f("message", "two\nlines"), f("last", " spaced")];
        for size in [input.len(), 1, 2, 3, 7, 64] {
            assert_eq!(collect(input, size).await, want, "chunks of {size}");
        }
    }

    #[tokio::test]
    async fn multibyte_characters_survive_being_split_across_chunks() {
        let input = "data: héllo 世界 🙂\n\n";
        assert_eq!(collect(input, 1).await, vec![f("message", "héllo 世界 🙂")]);
    }

    #[tokio::test]
    async fn an_unterminated_frame_is_dropped() {
        assert_eq!(collect("data: x\n\ndata: y", 5).await, vec![f("message", "x")]);
    }

    #[tokio::test]
    async fn a_frame_without_data_is_not_a_frame_and_resets_the_event_name() {
        assert_eq!(collect("event: ping\n\ndata: x\n\n", 4).await, vec![f("message", "x")]);
    }

    #[tokio::test]
    async fn the_consumer_stops_reading_by_dropping_the_stream() {
        let taken = Arc::new(AtomicUsize::new(0));
        let counter = taken.clone();
        let body = stream::iter(["data: a\n\n", "data: b\n\n", "data: c\n\n"]).map(move |c| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(c.as_bytes().to_vec())
        });
        let mut s = Box::pin(frames(body));
        assert_eq!(s.next().await.unwrap().unwrap(), f("message", "a"));
        drop(s);
        assert_eq!(taken.load(Ordering::SeqCst), 1, "nothing is read ahead of the consumer");
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused() {
        let input = format!("data: {}\n\n", "a".repeat(MAX_FRAME_BYTES + 1));
        let mut s = std::pin::pin!(frames(chunked(&input, 16 * 1024)));
        assert!(matches!(s.next().await, Some(Err(Error::FrameTooLarge))));
        assert!(s.next().await.is_none(), "the stream ends after the error");
    }

    #[tokio::test]
    async fn a_frame_made_of_many_small_lines_counts_as_one() {
        let line = format!("data: {}\n", "a".repeat(998));
        let input = line.repeat(MAX_FRAME_BYTES / line.len() + 2);
        let mut s = std::pin::pin!(frames(chunked(&input, 4096)));
        assert!(matches!(s.next().await, Some(Err(Error::FrameTooLarge))));
    }

    #[tokio::test]
    async fn a_frame_exactly_at_the_limit_is_accepted() {
        let data = "a".repeat(MAX_FRAME_BYTES - "data: \n\n".len());
        let input = format!("data: {data}\n\n");
        assert_eq!(input.len(), MAX_FRAME_BYTES);
        assert_eq!(collect(&input, 8192).await.len(), 1);
    }

    #[tokio::test]
    async fn frames_before_an_oversized_line_are_still_delivered() {
        let input = format!("data: ok\n\ndata: {}", "a".repeat(MAX_FRAME_BYTES));
        let mut s = std::pin::pin!(frames(chunked(&input, MAX_FRAME_BYTES + 100)));
        assert_eq!(s.next().await.unwrap().unwrap(), f("message", "ok"));
        assert!(matches!(s.next().await, Some(Err(Error::FrameTooLarge))));
    }

    #[tokio::test]
    async fn a_read_error_is_reported_after_the_frames_before_it() {
        let body = stream::iter(vec![Ok(b"data: a\n\n".to_vec()), Err("reset")]);
        let mut s = std::pin::pin!(frames(body));
        assert_eq!(s.next().await.unwrap().unwrap(), f("message", "a"));
        assert!(matches!(s.next().await, Some(Err(Error::Read("reset")))));
        assert!(s.next().await.is_none());
    }

    /// An upstream that sends bytes and never a newline, for as long as it is read. It stops by itself after eight
    /// budgets so a missing bound fails the test instead of exhausting memory.
    #[tokio::test]
    async fn a_line_that_never_ends_is_cut_off_not_buffered() {
        const CHUNK: usize = 8 * 1024;
        let taken = Arc::new(AtomicUsize::new(0));
        let counter = taken.clone();
        let body = stream::unfold((), move |()| {
            let counter = counter.clone();
            async move {
                if counter.load(Ordering::SeqCst) > 8 * MAX_FRAME_BYTES {
                    return None;
                }
                counter.fetch_add(CHUNK, Ordering::SeqCst);
                Some((Ok::<_, Infallible>(vec![b'a'; CHUNK]), ()))
            }
        });
        let mut s = std::pin::pin!(frames(body));
        assert!(matches!(s.next().await, Some(Err(Error::FrameTooLarge))));
        let read = taken.load(Ordering::SeqCst);
        assert!(read <= MAX_FRAME_BYTES + 128 * 1024, "read {read} bytes before giving up");
    }

    #[test]
    fn the_parser_never_holds_more_than_the_budget() {
        let mut p = Parser::with_limit(100);
        let mut out = VecDeque::new();
        assert!(p.push(&[b'a'; 100], &mut out).is_ok());
        assert_eq!(p.line.len(), 100);
        assert_eq!(p.push(b"a", &mut out), Err(FrameTooLarge));
        assert_eq!(p.line.len(), 100, "the overflowing byte was not copied");
    }
}
