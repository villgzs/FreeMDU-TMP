//! WebSocket client for the [`wifi_bridge`](https://github.com/medusalix/FreeMDU/tree/main/home) firmware `/optical` endpoint.
//!
//! Binary WebSocket messages carry raw optical/UART bytes in order (same framing as the USB bridge).

extern crate std;

use std::collections::VecDeque;

use embedded_io_async::{Error as IoErrorTrait, ErrorType, Read, ReadExactError, Write};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, trace};
use thiserror::Error;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::Message as WsMessage,
};

/// Errors from the WebSocket bridge client.
#[derive(Debug, Error)]
pub enum WsBridgeError {
    /// TCP or other I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// WebSocket protocol error.
    #[error("WebSocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
}

impl IoErrorTrait for WsBridgeError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Inner {
    stream: WsStream,
    rx_buf: VecDeque<u8>,
}

/// Async port type compatible with [`freemdu::device::connect`](crate::device::connect) and [`Interface`](crate::Interface).
pub struct WsBridgePort {
    inner: TokioMutex<Inner>,
}

impl ErrorType for WsBridgePort {
    type Error = WsBridgeError;
}

impl WsBridgePort {
    /// Connect to `ws://host:port/optical` (or `wss://` if supported by the server).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self, WsBridgeError> {
        let url = url.as_ref();
        info!("WebSocket client: connecting to {url}");
        let (stream, response) = tokio_tungstenite::connect_async(url).await?;
        info!(
            "WebSocket client: handshake OK (HTTP {})",
            response.status()
        );
        Ok(Self {
            inner: TokioMutex::new(Inner {
                stream,
                rx_buf: VecDeque::new(),
            }),
        })
    }
}

impl Read for WsBridgePort {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut inner = self.inner.lock().await;

        if inner.rx_buf.is_empty() {
            loop {
                trace!("WebSocket client: waiting for next frame…");
                match inner.stream.next().await.transpose()? {
                    Some(WsMessage::Binary(data)) => {
                        trace!(
                            "WebSocket client: binary frame ({} bytes) → optical RX",
                            data.len()
                        );
                        inner.rx_buf.extend(data.iter().copied());
                        break;
                    }
                    Some(WsMessage::Ping(p)) => {
                        debug!("WebSocket client: ping ({} bytes) → pong", p.len());
                        inner.stream.send(WsMessage::Pong(p)).await?;
                    }
                    Some(WsMessage::Pong(_)) => {
                        trace!("WebSocket client: pong");
                    }
                    Some(WsMessage::Close(_)) | None => return Ok(0),
                    Some(WsMessage::Text(t)) => {
                        debug!(
                            "WebSocket client: ignoring text frame ({} bytes)",
                            t.len()
                        );
                    }
                    Some(WsMessage::Frame(_)) => {
                        debug!("WebSocket client: ignoring raw frame");
                    }
                }
            }
        }

        let n = buf.len().min(inner.rx_buf.len());
        for (i, b) in inner.rx_buf.drain(..n).enumerate() {
            buf[i] = b;
        }

        Ok(n)
    }

    async fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), ReadExactError<Self::Error>> {
        while !buf.is_empty() {
            let n = self.read(buf).await.map_err(ReadExactError::Other)?;

            if n == 0 {
                return Err(ReadExactError::UnexpectedEof);
            }

            buf = &mut buf[n..];
        }

        Ok(())
    }
}

impl Write for WsBridgePort {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut inner = self.inner.lock().await;
        inner
            .stream
            .send(WsMessage::Binary(buf.to_vec().into()))
            .await?;
        Ok(buf.len())
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        let mut inner = self.inner.lock().await;
        inner
            .stream
            .send(WsMessage::Binary(buf.to_vec().into()))
            .await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut inner = self.inner.lock().await;
        inner.stream.flush().await?;
        Ok(())
    }
}
