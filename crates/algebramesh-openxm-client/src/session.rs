use std::net::SocketAddr;
use std::time::Duration;

use algebramesh_openxm_protocol::{
    Cmo, DecodeLimits, EncodeError, OxBody, OxMessage, SmCommand, WireOrder,
};
use tokio::net::TcpStream;

use crate::channel::{Channel, OxChannel};
use crate::error::OxError;
use crate::mathcap::Mathcap;

#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    pub limits: DecodeLimits,
    /// Upper bound for byte-order negotiation, which on the data channel includes the
    /// start-up time of the server process.
    pub handshake_timeout: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            limits: DecodeLimits::default(),
            handshake_timeout: Duration::from_secs(30),
        }
    }
}

/// One OpenXM server reached through a control channel and a data channel.
pub struct OxSession {
    control: OxChannel,
    data: OxChannel,
    peer: SocketAddr,
    next_serial: u32,
    poisoned: Option<String>,
}

impl OxSession {
    /// Negotiates byte order on both channels. The launcher answers on the control
    /// channel right away; the data channel answers once the server process is up.
    pub(crate) async fn establish(
        control: TcpStream,
        data: TcpStream,
        options: SessionOptions,
    ) -> Result<Self, OxError> {
        let peer = control.peer_addr().map_err(|source| OxError::Io {
            channel: Channel::Control,
            source,
        })?;
        let handshake = async {
            let control = OxChannel::handshake(Channel::Control, control, options.limits).await?;
            let data = OxChannel::handshake(Channel::Data, data, options.limits).await?;
            Ok::<_, OxError>((control, data))
        };
        let (control, data) = tokio::time::timeout(options.handshake_timeout, handshake)
            .await
            .map_err(|_| OxError::Timeout {
                after: options.handshake_timeout,
                during: "negotiating byte order",
            })??;
        tracing::debug!(%peer, data_order = ?data.wire_order(), "OpenXM session established");
        Ok(Self {
            control,
            data,
            peer,
            next_serial: 0,
            poisoned: None,
        })
    }

    /// Address of the launcher's control connection.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    pub fn wire_order(&self) -> WireOrder {
        self.data.wire_order()
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    fn check_usable(&self) -> Result<(), OxError> {
        match &self.poisoned {
            Some(reason) => Err(OxError::Poisoned(reason.clone())),
            None => Ok(()),
        }
    }

    fn serial(&mut self) -> u32 {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1);
        serial
    }

    fn record<T>(&mut self, result: Result<T, OxError>) -> Result<T, OxError> {
        if let Err(e) = &result
            && e.poisons_session()
            && self.poisoned.is_none()
        {
            self.poisoned = Some(e.to_string());
        }
        result
    }

    async fn send_data(&mut self, body: OxBody) -> Result<(), OxError> {
        self.check_usable()?;
        let message = OxMessage {
            serial: self.serial(),
            body,
        };
        let result = self.data.send(&message).await;
        self.record(result)
    }

    /// Pushes one object onto the server's operand stack.
    pub async fn push(&mut self, cmo: Cmo) -> Result<(), OxError> {
        self.send_data(OxBody::Data(cmo)).await
    }

    /// Sends a Stack Machine command on the data channel without waiting for output.
    pub async fn command(&mut self, command: SmCommand) -> Result<(), OxError> {
        self.send_data(OxBody::Command(command.code())).await
    }

    /// Sends `SM_popCMO` and waits for the resulting `OX_DATA`.
    pub async fn pop_cmo(&mut self) -> Result<Cmo, OxError> {
        self.command(SmCommand::PopCmo).await?;
        let result = self.receive_data().await;
        self.record(result)
    }

    async fn receive_data(&mut self) -> Result<Cmo, OxError> {
        match self.data.recv().await?.body {
            OxBody::Data(cmo) => Ok(cmo),
            OxBody::Command(code) => Err(OxError::UnexpectedMessage {
                channel: Channel::Data,
                detail: format!("OX_COMMAND {code} while waiting for OX_DATA"),
            }),
            OxBody::SyncBall => Err(OxError::UnexpectedMessage {
                channel: Channel::Data,
                detail: "OX_SYNC_BALL while waiting for OX_DATA".into(),
            }),
        }
    }

    /// Queries the server capabilities with `SM_mathcap`.
    pub async fn mathcap(&mut self) -> Result<Mathcap, OxError> {
        self.command(SmCommand::Mathcap).await?;
        let cmo = self.pop_cmo().await?;
        let result = Mathcap::from_cmo(cmo);
        self.record(result)
    }

    /// Calls `name(args...)` with `SM_executeFunction` and pops the result.
    ///
    /// Following the stack order of the specification the arguments are pushed last
    /// to first, then the argument count as `CMO_INT32`, then the function name.
    /// An error object returned by the server becomes [`OxError::Server`]; the stack
    /// is consistent afterwards because the error object has been popped.
    pub async fn execute_function(&mut self, name: &str, args: Vec<Cmo>) -> Result<Cmo, OxError> {
        let argc = i32::try_from(args.len()).map_err(|_| EncodeError::TooLong {
            what: "argument list",
            size: args.len(),
        })?;
        for arg in args.into_iter().rev() {
            self.push(arg).await?;
        }
        self.push(Cmo::Int32(argc)).await?;
        self.push(Cmo::string(name)).await?;
        self.command(SmCommand::ExecuteFunction).await?;
        match self.pop_cmo().await? {
            error @ Cmo::Error2(_) => Err(OxError::Server(error)),
            value => Ok(value),
        }
    }

    /// [`Self::execute_function`] with a deadline. On timeout the session is
    /// poisoned, because the server may still be computing and its stack state is
    /// unknown; it must be discarded rather than reused (design §5.4).
    pub async fn execute_function_timeout(
        &mut self,
        name: &str,
        args: Vec<Cmo>,
        timeout: Duration,
    ) -> Result<Cmo, OxError> {
        let outcome = tokio::time::timeout(timeout, self.execute_function(name, args)).await;
        self.finish_timed(outcome, timeout, "executing a function")
    }

    /// [`Self::mathcap`] with a deadline; see [`Self::execute_function_timeout`].
    pub async fn mathcap_timeout(&mut self, timeout: Duration) -> Result<Mathcap, OxError> {
        let outcome = tokio::time::timeout(timeout, self.mathcap()).await;
        self.finish_timed(outcome, timeout, "querying the mathcap")
    }

    fn finish_timed<T>(
        &mut self,
        outcome: Result<Result<T, OxError>, tokio::time::error::Elapsed>,
        after: Duration,
        during: &'static str,
    ) -> Result<T, OxError> {
        match outcome {
            Ok(result) => result,
            Err(_) => {
                let error = OxError::Timeout { after, during };
                self.poison(error.to_string());
                Err(error)
            }
        }
    }

    /// Marks the session unusable, for example after an abandoned request.
    pub fn poison(&mut self, reason: impl Into<String>) {
        if self.poisoned.is_none() {
            self.poisoned = Some(reason.into());
        }
    }

    /// Asks the launcher to terminate the server and itself (`SM_shutdown` on the
    /// control channel). Both connections are closed afterwards.
    pub async fn shutdown(mut self) -> Result<(), OxError> {
        let message = OxMessage::command(self.serial(), SmCommand::Shutdown);
        self.control.send(&message).await
    }

    /// Kills the server process (`SM_control_kill`), then shuts the launcher down.
    pub async fn kill(mut self) -> Result<(), OxError> {
        let kill = OxMessage::command(self.serial(), SmCommand::ControlKill);
        self.control.send(&kill).await?;
        self.shutdown().await
    }
}
