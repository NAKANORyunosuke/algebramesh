//! An in-process stand-in for `ox_launch` + an OpenXM server, for tests without a
//! real CAS. It connects back like `ox_launch` (control first, then data), takes the
//! server role in byte-order negotiation and runs a minimal Stack Machine.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use algebramesh_openxm_protocol::{
    ByteOrderByte, Cmo, DecodeLimits, OxBody, OxMessage, SmCommand, negotiate,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::channel::{Channel, OxChannel};

pub type Handler = Arc<dyn Fn(&str, Vec<Cmo>) -> Result<Cmo, String> + Send + Sync>;

#[derive(Clone)]
pub struct FakeServer {
    /// Byte the server announces during negotiation.
    pub byte_order: ByteOrderByte,
    pub mathcap: Cmo,
    pub handler: Handler,
    /// Delay before answering `SM_executeFunction`, to exercise timeouts.
    pub delay: Option<Duration>,
}

impl FakeServer {
    pub fn new(handler: Handler) -> Self {
        Self {
            byte_order: ByteOrderByte::LittleEndian,
            mathcap: default_mathcap(),
            handler,
            delay: None,
        }
    }

    /// Connects to the listener and serves until `SM_shutdown` or disconnection.
    pub async fn run(self, control: SocketAddr, data: SocketAddr) -> io::Result<()> {
        let control = TcpStream::connect(control).await?;
        let data = TcpStream::connect(data).await?;
        let mut control = self.handshake(Channel::Control, control).await?;
        let mut data = self.handshake(Channel::Data, data).await?;

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            while let Ok(message) = control.recv().await {
                if message.body == OxBody::Command(SmCommand::Shutdown.code()) {
                    break;
                }
            }
            let _ = stop_tx.send(());
        });

        let mut stack: Vec<Cmo> = Vec::new();
        let mut serial = 0u32;
        loop {
            let message = tokio::select! {
                _ = &mut stop_rx => return Ok(()),
                m = data.recv() => match m {
                    Ok(m) => m,
                    Err(_) => return Ok(()),
                },
            };
            match message.body {
                OxBody::Data(cmo) => stack.push(cmo),
                OxBody::SyncBall => {}
                OxBody::Command(code) => match SmCommand::from_code(code) {
                    Some(SmCommand::Mathcap) => stack.push(self.mathcap.clone()),
                    Some(SmCommand::ExecuteFunction) => {
                        if let Some(delay) = self.delay {
                            tokio::time::sleep(delay).await;
                        }
                        let result = self.execute(&mut stack, message.serial);
                        stack.push(result);
                    }
                    Some(SmCommand::PopCmo) => {
                        let value = stack.pop().unwrap_or(Cmo::Null);
                        serial += 1;
                        if data.send(&OxMessage::data(serial, value)).await.is_err() {
                            return Ok(());
                        }
                    }
                    Some(SmCommand::Shutdown) => return Ok(()),
                    _ => {}
                },
            }
        }
    }

    fn execute(&self, stack: &mut Vec<Cmo>, serial: u32) -> Cmo {
        let error = |msg: String| {
            Cmo::Error2(Box::new(Cmo::List(vec![
                Cmo::Int32(serial as i32),
                Cmo::String(msg),
            ])))
        };
        let Some(Cmo::String(name)) = stack.pop() else {
            return error("executeFunction : invalid function name".into());
        };
        let Some(argc) = stack.pop().and_then(|c| c.as_u32()) else {
            return error("executeFunction : invalid argc".into());
        };
        let mut args = Vec::new();
        for _ in 0..argc {
            args.push(stack.pop().unwrap_or(Cmo::Null));
        }
        match (self.handler)(&name, args) {
            Ok(value) => value,
            Err(msg) => error(msg),
        }
    }

    async fn handshake(&self, kind: Channel, mut stream: TcpStream) -> io::Result<OxChannel> {
        stream.write_all(&[self.byte_order.to_byte()]).await?;
        stream.flush().await?;
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        let order = negotiate(self.byte_order, byte[0])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(OxChannel::negotiated(
            kind,
            stream,
            order,
            DecodeLimits::default(),
        ))
    }
}

/// A mathcap in the layout used by `ox_asir`.
pub fn default_mathcap() -> Cmo {
    use algebramesh_openxm_protocol::cmo_tag::*;
    let ints = |v: &[u32]| Cmo::List(v.iter().map(|x| Cmo::Int32(*x as i32)).collect());
    Cmo::Mathcap(Box::new(Cmo::List(vec![
        Cmo::List(vec![
            Cmo::Int32(20_000_000),
            Cmo::string("Ox_system=fake_ox_server"),
            Cmo::string("Version=0"),
            Cmo::string("HOSTTYPE=test"),
        ]),
        ints(&[262, 264, 269, 272]),
        Cmo::List(vec![Cmo::List(vec![
            Cmo::Int32(514),
            ints(&[
                NULL,
                INT32,
                STRING,
                MATHCAP,
                LIST,
                ZZ,
                QQ,
                ZERO,
                RECURSIVE_POLYNOMIAL,
                UNIVARIATE_POLYNOMIAL,
                INDETERMINATE,
                ERROR2,
            ]),
        ])]),
    ])))
}
