use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use socks5_proto::{handshake, Address, Command, Error, Reply, Request, Response};
use socks5_server::{
    auth::NoAuth, connection::state::NeedAuthenticate, IncomingConnection, Server as Socks5Server,
};
use tokio::{
    io::{copy, split, AsyncWriteExt},
    join,
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::Notify,
};

use serde::Deserialize;

use getrandom::getrandom;
use rand_core::{RngCore, SeedableRng};
use rand_pcg::Pcg64;

use utils::*;

#[derive(Deserialize)]
pub struct Config {
    listen: String,
    up: String,
    down: String,
}

pub async fn start(config: Config) {
    let listener = TcpListener::bind(&config.listen).await.unwrap();
    let server = Socks5Server::new(listener, Arc::new(NoAuth) as Arc<_>);

    let (up, down) = (
        config.up.to_socket_addrs().unwrap().next().unwrap(),
        config.down.to_socket_addrs().unwrap().next().unwrap(),
    );

    let mut seed = [0; 32];
    getrandom(&mut seed).unwrap();
    let mut rng = Pcg64::from_seed(seed);

    loop {
        if let Ok((conn, _)) = server.accept().await {
            eprintln!("Connection from {}", conn.peer_addr().unwrap());

            let id = rng.next_u64();

            spawn(async move {
                if let Err(err) = handle(conn, up, down, id << 1).await {
                    eprintln!("{}", err)
                }
            });
        }
    }
}

async fn handle(
    conn: IncomingConnection<(), NeedAuthenticate>,
    up: SocketAddr,
    down: SocketAddr,
    id: u64,
) -> Result<(), Error> {
    let (conn, addr) = handle_socks5(conn).await?;

    let (res_up, res_down) = join!(
        dial_upstream(up, addr.clone()),
        dial_upstream(down, addr.clone())
    );
    let ((r_up, mut up_s), (r_down, mut down_s)) = (res_up?, res_down?);

    let (reply, err_from) = if r_up != Reply::Succeeded {
        (r_up, up.to_string())
    } else if r_down != Reply::Succeeded {
        (r_down, down.to_string())
    } else {
        (Reply::Succeeded, "".into())
    };

    let mut conn = match conn.reply(reply, addr).await {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err.into());
        }
    };
    if reply != Reply::Succeeded {
        let _ = conn.shutdown().await;

        return Err(other_error(
            format!("Unsuccessful socks5 reply from {}", err_from).as_str(),
        ));
    }

    let (mut up_c, mut down_c) = split(conn);

    let conn_closed = Arc::new(Notify::new());
    {
        let conn_closed = conn_closed.clone();
        spawn(async move {
            if down_s.write_u64(id | 1).await.is_ok() {
                select! {
                    _ = copy(&mut down_s, &mut down_c) => {},
                    _ = conn_closed.notified() => {}
                };
            }
            let _ = down_c.shutdown().await;
        });
    }

    if up_s.write_u64(id).await.is_ok() {
        let _ = copy(&mut up_c, &mut up_s).await;
    }

    conn_closed.notify_one();

    Ok(())
}

async fn dial_upstream(upstream: SocketAddr, addr: Address) -> Result<(Reply, TcpStream), Error> {
    let mut upstream = TcpStream::connect(upstream).await?;

    {
        let req = handshake::Request::new(vec![handshake::Method::NONE]);
        req.write_to(&mut upstream).await?;

        let resp = handshake::Response::read_from(&mut upstream).await?;
        if resp.method != handshake::Method::NONE {
            let peer_addr = upstream.peer_addr().unwrap();
            let _ = upstream.shutdown().await;
            return Err(other_error(
                format!("No acceptable auth method from {}", peer_addr).as_str(),
            ));
        }
    }

    let req = Request::new(Command::Connect, addr);
    req.write_to(&mut upstream).await?;

    let reply = Response::read_from(&mut upstream).await?.reply;
    if reply != Reply::Succeeded {
        let _ = upstream.shutdown().await;
    }

    Ok((reply, upstream))
}
