use std::{
    env::args,
    fs::File,
    io::{Error as IoError, ErrorKind},
};

use serde::de::DeserializeOwned;
use socks5_proto::{Address, Error, Reply};
use socks5_server::{
    connection::{
        connect::state::{NeedReply, Ready},
        state::NeedAuthenticate,
    },
    Command, Connect, IncomingConnection,
};

use tokio::{
    io::{split, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
};

pub fn other_error(err: &str) -> Error {
    IoError::new(ErrorKind::Other, err).into()
}

pub async fn handle_socks5(
    conn: IncomingConnection<(), NeedAuthenticate>,
) -> Result<(Connect<NeedReply>, Address), Error> {
    let conn = match conn.authenticate().await {
        Ok((conn, _)) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err);
        }
    };

    match conn.wait().await {
        Ok(Command::Associate(associate, _)) => {
            let replied = associate
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            let conn = match replied {
                Ok(conn) => conn,
                Err((err, mut conn)) => {
                    let _ = conn.shutdown().await;
                    return Err(err.into());
                }
            };

            let _ = conn.into_inner().shutdown().await;
        }
        Ok(Command::Bind(bind, _)) => {
            let replied = bind
                .reply(Reply::CommandNotSupported, Address::unspecified())
                .await;

            let conn = match replied {
                Ok(conn) => conn,
                Err((err, mut conn)) => {
                    let _ = conn.shutdown().await;
                    return Err(err.into());
                }
            };

            let _ = conn.into_inner().shutdown().await;
        }
        Ok(Command::Connect(connect, addr)) => {
            return Ok((connect, addr));
        }
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err);
        }
    }

    Err(other_error("Unimplemented command"))
}

pub async fn connect_remote(
    addr: Address,
    w: &mut WriteHalf<Connect<Ready>>,
) -> Result<(ReadHalf<TcpStream>, WriteHalf<TcpStream>), Error> {
    let conn_remote = match addr.clone() {
        Address::SocketAddress(addr) => TcpStream::connect(addr).await,
        Address::DomainAddress(host, port) => {
            TcpStream::connect((String::from_utf8_lossy(&host).as_ref(), port)).await
        }
    };

    let (down, up) = match conn_remote {
        Ok(conn) => split(conn),
        Err(err) => {
            let _ = w.shutdown().await;
            return Err(err.into());
        }
    };

    Ok((down, up))
}

pub fn config_from_arg<T: DeserializeOwned>() -> T {
    config_from_path(&args().nth(1).unwrap())
}

pub fn config_from_path<T: DeserializeOwned>(path: &str) -> T {
    serde_json::from_reader(File::open(path).unwrap()).unwrap()
}
