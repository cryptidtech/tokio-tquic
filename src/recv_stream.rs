use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncRead;
use tokio::sync::oneshot;

use crate::bridge::{BridgeCommand, BridgeHandle};
use crate::error::ConnectionError;
use crate::send_stream::ClosedStream;
use crate::{StreamId, VarInt};

/// A stream that can be used to receive data from the peer.
#[derive(Debug)]
pub struct RecvStream {
    pub(crate) conn_id: u64,
    pub(crate) stream_id: u64,
    pub(crate) handle: Arc<BridgeHandle>,

    pub(crate) all_data_read: bool,
}

impl RecvStream {
    /// Read data from the stream.
    ///
    /// Returns `Ok(Some(n))` if `n` bytes were read, or `Ok(None)` if the stream is finished.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        if self.all_data_read {
            return Ok(None);
        }

        loop {
            let (tx, rx) = oneshot::channel();
            self.handle.send_cmd(BridgeCommand::StreamRead {
                conn_id: self.conn_id,
                stream_id: self.stream_id,
                max_len: buf.len(),
                respond: tx,
            });

            match rx.await {
                Ok(Ok((data, fin))) => {
                    if data.is_empty() && !fin {
                        // TQUIC returned Done (no data available). Wait for readable.
                        self.wait_readable().await?;
                        continue;
                    }

                    let n = data.len();
                    buf[..n].copy_from_slice(&data);

                    if fin {
                        self.all_data_read = true;
                        if n == 0 {
                            return Ok(None);
                        }
                    }

                    return Ok(Some(n));
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(ReadError::ConnectionLost(ConnectionError::LocallyClosed));
                }
            }
        }
    }

    /// Read exactly `buf.len()` bytes from the stream.
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        let mut offset = 0;
        while offset < buf.len() {
            match self.read(&mut buf[offset..]).await {
                Ok(Some(n)) => offset += n,
                Ok(None) => return Err(ReadExactError::FinishedEarly(offset)),
                Err(e) => return Err(ReadExactError::ReadError(e)),
            }
        }
        Ok(())
    }

    /// Read all remaining data from the stream.
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        let mut data = Vec::new();
        let mut buf = vec![0u8; 8192];
        loop {
            match self.read(&mut buf).await {
                Ok(Some(n)) => {
                    data.extend_from_slice(&buf[..n]);
                    if data.len() > size_limit {
                        return Err(ReadToEndError::TooLong);
                    }
                }
                Ok(None) => return Ok(data),
                Err(e) => return Err(ReadToEndError::Read(e)),
            }
        }
    }

    /// Stop reading from the stream, signaling to the peer.
    pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        if self.all_data_read {
            return Err(ClosedStream);
        }
        self.all_data_read = true;

        self.handle.send_cmd(BridgeCommand::StreamShutdownRead {
            conn_id: self.conn_id,
            stream_id: self.stream_id,
            error_code: error_code.into_inner(),
        });

        Ok(())
    }

    /// Get the stream's identifier.
    pub fn id(&self) -> StreamId {
        StreamId::from_raw(self.stream_id)
    }

    /// Wait until the stream becomes readable.
    async fn wait_readable(&self) -> Result<(), ReadError> {
        let cn = self
            .handle
            .get_conn_notify(self.conn_id)
            .ok_or(ReadError::ConnectionLost(ConnectionError::LocallyClosed))?;

        // Check if connection is already closed.
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Err(ReadError::ConnectionLost(err));
        }

        let notify = cn.get_or_create_readable(self.stream_id);
        notify.notified().await;

        // Check connection state after wakeup.
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Err(ReadError::ConnectionLost(err));
        }

        Ok(())
    }
}

// Note: RecvStream implements Drop, which is incompatible with pin_project_lite.
// All fields are Unpin (u64, Arc, bool), so Pin::get_mut() is sound here.
impl AsyncRead for RecvStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if this.all_data_read {
            return Poll::Ready(Ok(()));
        }

        let max_len = buf.remaining();
        let (tx, rx) = oneshot::channel();
        this.handle.send_cmd(BridgeCommand::StreamRead {
            conn_id: this.conn_id,
            stream_id: this.stream_id,
            max_len,
            respond: tx,
        });

        let waker = cx.waker().clone();
        let conn_id = this.conn_id;
        let stream_id = this.stream_id;
        let handle = this.handle.clone();

        tokio::spawn(async move {
            match rx.await {
                Ok(Ok((data, _fin))) => {
                    if data.is_empty() {
                        // Wait for readable then wake.
                        if let Some(cn) = handle.get_conn_notify(conn_id) {
                            let notify = cn.get_or_create_readable(stream_id);
                            notify.notified().await;
                        }
                    }
                    waker.wake();
                }
                _ => waker.wake(),
            }
        });

        Poll::Pending
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.all_data_read {
            let _ = self.stop(VarInt::from(0u32));
        }
    }
}

/// Errors from reading a [`RecvStream`].
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The stream was reset by the peer.
    #[error("stream reset by peer: error {0}")]
    Reset(VarInt),
    /// The connection was lost.
    #[error("connection lost: {0}")]
    ConnectionLost(#[from] ConnectionError),
    /// The stream was already stopped or finished.
    #[error("closed stream")]
    ClosedStream,
    /// 0-RTT data was rejected.
    #[error("0-RTT rejected")]
    ZeroRttRejected,
}

/// Errors from [`RecvStream::read_exact`].
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ReadExactError {
    /// The stream finished before enough data was received.
    #[error("stream finished early ({0} bytes read)")]
    FinishedEarly(usize),
    /// A read error occurred.
    #[error(transparent)]
    ReadError(#[from] ReadError),
}

/// Errors from [`RecvStream::read_to_end`].
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ReadToEndError {
    /// A read error occurred.
    #[error("read error: {0}")]
    Read(#[from] ReadError),
    /// The stream exceeded the size limit.
    #[error("stream too long")]
    TooLong,
}

/// Errors from waiting for a stream reset.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ResetError {
    /// The connection was lost.
    #[error("connection lost: {0}")]
    ConnectionLost(#[from] ConnectionError),
    /// 0-RTT data was rejected.
    #[error("0-RTT rejected")]
    ZeroRttRejected,
}
