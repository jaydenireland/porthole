//! Steps past the control server's optional opening challenge.
//!
//! Right after the handshake the server may send a challenge: the bytes
//! `FF FF FF 'T' 'S'`, a four-byte length, and that much JSON. If it does not,
//! the first bytes are already HTTP/2.
//!
//! tailscale-rs handles this by reading the first bytes before it starts
//! HTTP/2. That works over TCP, where the server always speaks first. Over a
//! WebSocket the server sends no challenge and waits for the client's HTTP/2
//! greeting, so reading first would wait forever. This wrapper makes the same
//! check on whatever arrives first, without holding up what we send.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const MAGIC: [u8; 5] = [0xFF, 0xFF, 0xFF, b'T', b'S'];
const HEADER_LEN: usize = MAGIC.len() + 4;
/// The same limit tailscale-rs applies.
const MAX_CHALLENGE_LEN: usize = 1024;

enum State {
    /// Collecting the first bytes to see whether they are a challenge.
    Start { seen: [u8; HEADER_LEN], len: usize },
    /// Throwing away the rest of a challenge.
    Discard { left: usize },
    /// The first bytes were not a challenge; hand them to the reader.
    Replay { seen: [u8; HEADER_LEN], from: usize, len: usize },
    /// Everything from here on is the reader's.
    Through,
}

pub struct SkipChallenge<T> {
    inner: T,
    state: State,
}

impl<T> SkipChallenge<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, state: State::Start { seen: [0; HEADER_LEN], len: 0 } }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for SkipChallenge<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            match &mut this.state {
                State::Start { seen, len } => {
                    // Until the marker has matched, read no further than its
                    // end, so nothing beyond it needs handing back.
                    let want = if *len < MAGIC.len() { MAGIC.len() } else { HEADER_LEN };
                    let mut buf = ReadBuf::new(&mut seen[*len..want]);
                    ready!(Pin::new(&mut this.inner).poll_read(cx, &mut buf))?;
                    let got = buf.filled().len();
                    *len += got;

                    let is_challenge = seen[..(*len).min(MAGIC.len())] == MAGIC[..(*len).min(MAGIC.len())];
                    if got == 0 || !is_challenge {
                        // The stream ended, or this is not a challenge.
                        this.state = State::Replay { seen: *seen, from: 0, len: *len };
                    } else if *len == HEADER_LEN {
                        let left = u32::from_be_bytes([seen[5], seen[6], seen[7], seen[8]]) as usize;
                        if left > MAX_CHALLENGE_LEN {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "the control server's challenge is too long",
                            )));
                        }
                        this.state = State::Discard { left };
                    }
                }
                State::Discard { left } => {
                    if *left == 0 {
                        this.state = State::Through;
                        continue;
                    }
                    let mut scratch = [0u8; 256];
                    let take = (*left).min(scratch.len());
                    let mut buf = ReadBuf::new(&mut scratch[..take]);
                    ready!(Pin::new(&mut this.inner).poll_read(cx, &mut buf))?;
                    let got = buf.filled().len();
                    if got == 0 {
                        this.state = State::Through;
                    } else {
                        *left -= got;
                    }
                }
                State::Replay { seen, from, len } => {
                    if *from == *len {
                        this.state = State::Through;
                        continue;
                    }
                    let take = (*len - *from).min(out.remaining());
                    out.put_slice(&seen[*from..*from + take]);
                    *from += take;
                    return Poll::Ready(Ok(()));
                }
                State::Through => return Pin::new(&mut this.inner).poll_read(cx, out),
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for SkipChallenge<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Feeds `chunks` through one at a time and returns what the reader sees.
    async fn read_through(chunks: &[&[u8]]) -> io::Result<Vec<u8>> {
        let (mut server, client) = tokio::io::duplex(64);
        let mut reader = SkipChallenge::new(client);
        let chunks: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        let writer = async move {
            for chunk in chunks {
                server.write_all(&chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        };
        let mut seen = Vec::new();
        let (_, result) = tokio::join!(writer, reader.read_to_end(&mut seen));
        result.map(|_| seen)
    }

    fn challenge(json: &[u8]) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend((json.len() as u32).to_be_bytes());
        bytes.extend(json);
        bytes
    }

    #[tokio::test]
    async fn passes_http2_through_untouched() {
        let settings = [0, 0, 18, 4, 0, 0, 0, 0, 0, 1, 2, 3];
        assert_eq!(read_through(&[&settings]).await.unwrap(), settings);
    }

    #[tokio::test]
    async fn drops_a_challenge() {
        let mut bytes = challenge(br#"{"nodeKeyChallenge":"chalpub:00"}"#);
        bytes.extend(b"after");
        assert_eq!(read_through(&[&bytes]).await.unwrap(), b"after");
    }

    #[tokio::test]
    async fn drops_a_challenge_that_arrives_in_pieces() {
        let mut bytes = challenge(&[b'x'; 600]);
        bytes.extend(b"after");
        let pieces: Vec<&[u8]> = bytes.chunks(7).collect();
        assert_eq!(read_through(&pieces).await.unwrap(), b"after");
    }

    #[tokio::test]
    async fn keeps_bytes_that_only_start_like_a_challenge() {
        let bytes = [0xFF, 0xFF, 0xFF, b'T', b'x', 1, 2, 3, 4, 5, 6];
        assert_eq!(read_through(&[&bytes[..2], &bytes[2..]]).await.unwrap(), bytes);
    }

    #[tokio::test]
    async fn keeps_a_stream_shorter_than_the_marker() {
        assert_eq!(read_through(&[&[0xFF, 0xFF]]).await.unwrap(), [0xFF, 0xFF]);
    }

    #[tokio::test]
    async fn empty_stream_is_empty() {
        assert_eq!(read_through(&[]).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn refuses_an_oversized_challenge() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(5000u32.to_be_bytes());
        assert!(read_through(&[&bytes]).await.is_err());
    }
}
