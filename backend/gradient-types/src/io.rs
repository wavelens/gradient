/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Combined async I/O trait used for type-erasing Nix daemon connections.
pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}

/// Type-erased wrapper over any bidirectional async I/O stream.
/// Lets callers hold a `DaemonStore<BoxedIo>` regardless of whether the
/// underlying transport is a Unix socket, a stdio pipe, or a TCP channel.
pub struct BoxedIo(Box<dyn AsyncIo>);

impl std::fmt::Debug for BoxedIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxedIo").finish_non_exhaustive()
    }
}

impl BoxedIo {
    pub fn new(io: impl AsyncIo + 'static) -> Self {
        Self(Box::new(io))
    }
}

impl AsyncRead for BoxedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}
