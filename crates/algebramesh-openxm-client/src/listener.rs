use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::net::TcpListener;

use crate::channel::Channel;
use crate::error::OxError;
use crate::session::{OxSession, SessionOptions};

/// Listener pair for the `ReverseConnect` method (design §5.2).
///
/// It plays the role of Asir's `try_bind_listen` + `try_accept`: `ox_launch` on the
/// worker connects first to the control port, then to the data port, and starts the
/// server process with the data connection as its stream. Port `0` asks the
/// operating system for a free port.
pub struct ReverseConnectListener {
    control: TcpListener,
    data: TcpListener,
    options: SessionOptions,
}

impl ReverseConnectListener {
    pub async fn bind(
        ip: IpAddr,
        control_port: u16,
        data_port: u16,
        options: SessionOptions,
    ) -> Result<Self, OxError> {
        let control = TcpListener::bind(SocketAddr::new(ip, control_port))
            .await
            .map_err(OxError::Bind)?;
        let data = TcpListener::bind(SocketAddr::new(ip, data_port))
            .await
            .map_err(OxError::Bind)?;
        Ok(Self {
            control,
            data,
            options,
        })
    }

    pub fn control_addr(&self) -> SocketAddr {
        local_addr(&self.control)
    }

    pub fn data_addr(&self) -> SocketAddr {
        local_addr(&self.data)
    }

    /// Arguments for `ox_launch` on the worker:
    /// `ox_launch <host> 0 <control_port> <data_port> <server> 0`, where `0` in the
    /// second position selects "connect" mode and the last `0` means "no display".
    pub fn launch_args(&self, host_seen_by_worker: &str, server: &str) -> Vec<String> {
        vec![
            host_seen_by_worker.to_owned(),
            "0".to_owned(),
            self.control_addr().port().to_string(),
            self.data_addr().port().to_string(),
            server.to_owned(),
            "0".to_owned(),
        ]
    }

    /// Accepts one control connection and one data connection from the same host and
    /// negotiates byte order on both. The listeners are closed afterwards, so a
    /// listener pair serves exactly one session. If either step fails, both
    /// connections are dropped.
    pub async fn accept(self, timeout: Duration) -> Result<OxSession, OxError> {
        let Self {
            control,
            data,
            options,
        } = self;
        let accepted = tokio::time::timeout(timeout, async {
            let (c, c_addr) = control.accept().await.map_err(|source| OxError::Io {
                channel: Channel::Control,
                source,
            })?;
            let (d, d_addr) = data.accept().await.map_err(|source| OxError::Io {
                channel: Channel::Data,
                source,
            })?;
            Ok::<_, OxError>((c, c_addr, d, d_addr))
        })
        .await
        .map_err(|_| OxError::Timeout {
            after: timeout,
            during: "waiting for ox_launch to connect",
        })??;
        drop((control, data));

        let (c, c_addr, d, d_addr) = accepted;
        if c_addr.ip() != d_addr.ip() {
            return Err(OxError::PeerMismatch {
                control: c_addr,
                data: d_addr,
            });
        }
        tracing::info!(control = %c_addr, data = %d_addr, "ox_launch connected");
        OxSession::establish(c, d, options).await
    }
}

fn local_addr(listener: &TcpListener) -> SocketAddr {
    listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)))
}
