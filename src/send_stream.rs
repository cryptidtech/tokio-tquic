use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::AsyncWrite;
use tokio::sync::oneshot;

use crate::bridge::{BridgeCommand, BridgeHandle};
use crate::error::ConnectionError;
use crate::{StreamId, VarInt};

/// A stream that can be used to send data to the peer.
#[derive(Debug)]
pub struct SendStream {
    pub(crate) conn_id: u64,
    pub(crate) stream_id: u64,
    pub(crate) handle: Arc<BridgeHandle>,

    pub(crate) finished: bool,
}

impl SendStream {
    /// Write data to the stream.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError> {
        if self.finished {
            return Err(WriteError::ClosedStream);
        }

        loop {
            let (tx, rx) = oneshot::channel();
            self.handle.send_cmd(BridgeCommand::StreamWrite {
                conn_id: self.conn_id,
                stream_id: self.stream_id,
                data: Bytes::copy_from_slice(buf),
                fin: false,
                respond: tx,
            });

            match rx.await {
                Ok(Ok(0)) => {
                    // TQUIC returned Done (no capacity). Wait for writable notification.
                    self.wait_writable().await?;
                }
                Ok(Ok(n)) => return Ok(n),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(WriteError::ConnectionLost(ConnectionError::LocallyClosed)),
            }
        }
    }

    /// Write all data to the stream.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        let mut offset = 0;
        while offset < buf.len() {
            let n = self.write(&buf[offset..]).await?;
            offset += n;
        }
        Ok(())
    }

    /// Signal that no more data will be sent on this stream.
    pub fn finish(&mut self) -> Result<(), ClosedStream> {
        if self.finished {
            return Err(ClosedStream);
        }
        self.finished = true;

        // Send a zero-length write with fin=true.
        let (tx, _rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::StreamWrite {
            conn_id: self.conn_id,
            stream_id: self.stream_id,
            data: Bytes::new(),
            fin: true,
            respond: tx,
        });

        Ok(())
    }

    /// Abort the stream, notifying the peer.
    pub fn reset(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        if self.finished {
            return Err(ClosedStream);
        }
        self.finished = true;

        self.handle.send_cmd(BridgeCommand::StreamShutdownWrite {
            conn_id: self.conn_id,
            stream_id: self.stream_id,
            error_code: error_code.into_inner(),
        });

        Ok(())
    }

    /// Set the send priority of this stream.
    pub fn set_priority(&self, priority: i32) -> Result<(), ClosedStream> {
        if self.finished {
            return Err(ClosedStream);
        }

        let urgency = priority.clamp(0, 255) as u8;
        self.handle.send_cmd(BridgeCommand::StreamSetPriority {
            conn_id: self.conn_id,
            stream_id: self.stream_id,
            urgency,
        });

        Ok(())
    }

    /// Get the stream's identifier.
    pub fn id(&self) -> StreamId {
        StreamId::from_raw(self.stream_id)
    }

    /// Wait until the stream becomes writable.
    async fn wait_writable(&self) -> Result<(), WriteError> {
        let cn = self
            .handle
            .get_conn_notify(self.conn_id)
            .ok_or(WriteError::ConnectionLost(ConnectionError::LocallyClosed))?;

        // Check if connection is already closed.
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Err(WriteError::ConnectionLost(err));
        }

        let notify = cn.get_or_create_writable(self.stream_id);
        notify.notified().await;

        // Check connection state after wakeup.
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Err(WriteError::ConnectionLost(err));
        }

        Ok(())
    }
}

// Note: SendStream implements Drop, which is incompatible with pin_project_lite.
// All fields are Unpin (u64, Arc, bool), so Pin::get_mut() is sound here.
impl AsyncWrite for SendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        if this.finished {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stream finished",
            )));
        }

        let (tx, rx) = oneshot::channel();
        this.handle.send_cmd(BridgeCommand::StreamWrite {
            conn_id: this.conn_id,
            stream_id: this.stream_id,
            data: Bytes::copy_from_slice(buf),
            fin: false,
            respond: tx,
        });

        // Try to get the response. Since the bridge processes commands quickly,
        // we attempt a synchronous check first.
        let waker = cx.waker().clone();
        let conn_id = this.conn_id;
        let stream_id = this.stream_id;
        let handle = this.handle.clone();

        tokio::spawn(async move {
            match rx.await {
                Ok(Ok(0)) => {
                    // Need to wait for writable, then wake.
                    if let Some(cn) = handle.get_conn_notify(conn_id) {
                        let notify = cn.get_or_create_writable(stream_id);
                        notify.notified().await;
                    }
                    waker.wake();
                }
                _ => waker.wake(),
            }
        });

        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.finish() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stream already closed",
            ))),
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish();
        }
    }
}

/// Errors from writing to a [`SendStream`].
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// The peer is no longer reading from this stream.
    #[error("sending stopped by peer: error {0}")]
    Stopped(VarInt),
    /// The connection was lost.
    #[error("connection lost: {0}")]
    ConnectionLost(#[from] ConnectionError),
    /// The stream has already been finished or reset.
    #[error("closed stream")]
    ClosedStream,
    /// 0-RTT data was rejected by the server.
    #[error("0-RTT rejected")]
    ZeroRttRejected,
}

/// Errors from waiting for a stream to be stopped.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum StoppedError {
    /// The connection was lost.
    #[error("connection lost: {0}")]
    ConnectionLost(#[from] ConnectionError),
    /// 0-RTT data was rejected.
    #[error("0-RTT rejected")]
    ZeroRttRejected,
}

/// Error indicating the stream is already closed.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("closed stream")]
pub struct ClosedStream;
