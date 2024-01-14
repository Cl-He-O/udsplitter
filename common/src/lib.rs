use std::io::{Error as IoError, ErrorKind};

use socks5_proto::{Address, Error, Reply};
use socks5_server::{connection::{connect::state::NeedReply, state::NeedAuthenticate}, Command, Connect, IncomingConnection};

use tokio::io::AsyncWriteExt;

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
