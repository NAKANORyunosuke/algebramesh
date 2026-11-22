//! Reverse-connect sessions against the in-process fake launcher/server.

#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use algebramesh_openxm_client::testing::FakeServer;
use algebramesh_openxm_client::{OxError, ReverseConnectListener, SessionOptions};
use algebramesh_openxm_protocol::{ByteOrderByte, Cmo, SmCommand, WireOrder};
use num_bigint::BigInt;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const TIMEOUT: Duration = Duration::from_secs(10);

fn adder() -> FakeServer {
    FakeServer::new(Arc::new(|name, args| match name {
        "add" => {
            let mut sum = BigInt::from(0);
            for arg in args {
                match arg {
                    Cmo::Zz(v) => sum += v,
                    Cmo::Int32(v) => sum += v,
                    other => return Err(format!("add: unexpected {other:?}")),
                }
            }
            Ok(Cmo::Zz(sum))
        }
        "args" => Ok(Cmo::List(args)),
        other => Err(format!("executeFunction : the function {other} not found")),
    }))
}

async fn connect(server: FakeServer) -> algebramesh_openxm_client::OxSession {
    let listener = ReverseConnectListener::bind(LOOPBACK, 0, 0, SessionOptions::default())
        .await
        .unwrap();
    let (c, d) = (listener.control_addr(), listener.data_addr());
    tokio::spawn(server.run(c, d));
    listener.accept(TIMEOUT).await.unwrap()
}

#[tokio::test]
async fn negotiates_network_order_even_if_server_prefers_little_endian() {
    let session = connect(adder()).await;
    assert_eq!(session.wire_order(), WireOrder::Big);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn mathcap_and_function_execution() {
    let mut session = connect(adder()).await;
    let mathcap = session.mathcap().await.unwrap();
    assert_eq!(mathcap.system.as_deref(), Some("fake_ox_server"));
    assert!(mathcap.supports_command(SmCommand::ExecuteFunction));

    let big = BigInt::parse_bytes(b"123456789012345678901234567890", 10).unwrap();
    let sum = session
        .execute_function("add", vec![Cmo::Zz(big.clone()), Cmo::Int32(10)])
        .await
        .unwrap();
    assert_eq!(sum, Cmo::Zz(big + 10));

    // Arguments arrive in call order.
    let echoed = session
        .execute_function(
            "args",
            vec![Cmo::string("a"), Cmo::List(vec![Cmo::Null]), Cmo::Int32(3)],
        )
        .await
        .unwrap();
    assert_eq!(
        echoed,
        Cmo::List(vec![
            Cmo::string("a"),
            Cmo::List(vec![Cmo::Null]),
            Cmo::Int32(3)
        ])
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_error_keeps_session_usable() {
    let mut session = connect(adder()).await;
    let err = session
        .execute_function("missing", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, OxError::Server(_)));
    assert!(err.to_string().contains("missing not found"));
    assert!(!session.is_poisoned());
    let sum = session
        .execute_function("add", vec![Cmo::Int32(1), Cmo::Int32(2)])
        .await
        .unwrap();
    assert_eq!(sum, Cmo::Zz(BigInt::from(3)));
}

#[tokio::test]
async fn timeout_poisons_session() {
    let mut server = adder();
    server.delay = Some(Duration::from_secs(5));
    let mut session = connect(server).await;
    let err = session
        .execute_function_timeout("add", vec![Cmo::Int32(1)], Duration::from_millis(100))
        .await
        .unwrap_err();
    assert!(matches!(err, OxError::Timeout { .. }));
    assert!(session.is_poisoned());
    let again = session.execute_function("add", vec![]).await.unwrap_err();
    assert!(matches!(again, OxError::Poisoned(_)));
    session.kill().await.unwrap();
}

#[tokio::test]
async fn big_endian_server_also_works() {
    let mut server = adder();
    server.byte_order = ByteOrderByte::BigEndian;
    let mut session = connect(server).await;
    let sum = session
        .execute_function("add", vec![Cmo::Int32(-5), Cmo::Int32(2)])
        .await
        .unwrap();
    assert_eq!(sum, Cmo::Zz(BigInt::from(-3)));
}

#[tokio::test]
async fn accept_times_out_without_worker() {
    let listener = ReverseConnectListener::bind(LOOPBACK, 0, 0, SessionOptions::default())
        .await
        .unwrap();
    let err = listener
        .accept(Duration::from_millis(100))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, OxError::Timeout { .. }));
}

#[tokio::test]
async fn spawn_failure_byte_is_reported() {
    // ox_launch writes 0x02 on the data channel when it cannot exec the server.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = ReverseConnectListener::bind(LOOPBACK, 0, 0, SessionOptions::default())
        .await
        .unwrap();
    let (c, d) = (listener.control_addr(), listener.data_addr());
    tokio::spawn(async move {
        let mut control = tokio::net::TcpStream::connect(c).await.unwrap();
        let mut data = tokio::net::TcpStream::connect(d).await.unwrap();
        control.write_all(&[0x01]).await.unwrap();
        let mut b = [0u8; 1];
        control.read_exact(&mut b).await.unwrap();
        data.write_all(&[0x02]).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let err = listener.accept(TIMEOUT).await.err().unwrap();
    assert!(err.to_string().contains("could not be spawned"), "{err}");
}
