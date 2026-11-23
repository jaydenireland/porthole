//! A WebSocket as a plain byte stream, which is what the Tailscale protocols
//! are written against.

use tokio::io::{AsyncRead, AsyncWrite};
use ws_stream_wasm::WsMeta;

use crate::{error::Error, local::Local};

pub trait Io: AsyncRead + AsyncWrite + Unpin {}
impl<T: AsyncRead + AsyncWrite + Unpin> Io for T {}

/// The byte stream of an open WebSocket.
pub type Stream = Local<Box<dyn Io>>;

/// Keeps a WebSocket open; dropping it closes the socket.
pub struct Socket(#[allow(dead_code)] WsMeta);

/// Open `url`, asking the server for `protocol`.
pub async fn open(url: &str, protocol: &str) -> Result<(Socket, Stream), Error> {
    let (meta, stream) = WsMeta::connect(url, vec![protocol]).await?;
    Ok((Socket(meta), Local(Box::new(stream.into_io()))))
}
