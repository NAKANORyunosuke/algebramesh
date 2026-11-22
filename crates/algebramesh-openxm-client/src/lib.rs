//! Asynchronous OpenXM client.
//!
//! * [`ReverseConnectListener`] binds a control port and a data port and waits for
//!   `ox_launch` to connect back, as Asir's `try_bind_listen`/`try_accept` do.
//! * [`spawn_local`] starts `ox_launch` on this host and accepts its connections.
//! * [`OxSession`] drives the Stack Machine of one server. It allows one request in
//!   flight at a time (every operation takes `&mut self`), and a session that hit a
//!   timeout or a protocol error is poisoned and refuses further use (design §5.4).

mod channel;
mod error;
mod listener;
mod mathcap;
mod session;
mod spawn;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use channel::Channel;
pub use error::{OxError, server_error_message};
pub use listener::ReverseConnectListener;
pub use mathcap::Mathcap;
pub use session::{OxSession, SessionOptions};
pub use spawn::{LocalLauncher, LocalSession, spawn_local};

pub use algebramesh_openxm_protocol as protocol;
