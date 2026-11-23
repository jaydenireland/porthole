//! A browser tab runs this code on one thread, but the libraries we build on
//! ask for `Send` connections because natively they hand them to a thread
//! pool. `Local` makes that promise for values that only ever live on the
//! tab's thread.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub struct Local<T>(pub T);

// SAFETY: wasm32-unknown-unknown without the `atomics` target feature has a
// single thread, so a value can never be reached from another one.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl<T> Send for Local<T> {}

impl<T: AsyncRead + Unpin> AsyncRead for Local<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Local<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
