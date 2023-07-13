use std::{
    env::args,
    fs::File,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use socks5_proto::{handshake, Address, Command, Error, Reply, Request, Response};
use socks5_server::{auth::NoAuth, IncomingConnection, Server as Socks5Server};
use tokio::{
    io::{copy, split, AsyncWriteExt},
    join,
    net::{TcpListener, TcpStream},
    spawn,
};

use serde::Deserialize;

use getrandom::getrandom;
use rand_core::{RngCore, SeedableRng};
use rand_pcg::Pcg64;

use common::{handle_socks5, other_error};

#[derive(Deserialize)]
struct Config {
    listen: String,
    up: String,
    down: String,
}

#[tokio::main]
async fn main() {
    let config: Config =
        serde_json::from_reader(File::open(args().nth(1).unwrap()).unwrap()).unwrap();

    let listener = TcpListener::bind(&config.listen).await.unwrap();
    let server = Socks5Server::from((listener, Arc::new(NoAuth) as Arc<_>));

    let (up, down) = (
        config.up.to_socket_addrs().unwrap().next().unwrap(),
        config.down.to_socket_addrs().unwrap().next().unwrap(),
    );

    let mut seed = [0; 32];
    getrandom(&mut seed).unwrap();
    let mut rng = Pcg64::from_seed(seed);

    loop {
        if let Ok((conn, _)) = server.accept().await {
            let peer_addr = conn.peer_addr().unwrap();
            eprintln!("Connection from {}", peer_addr);

            let id = rng.next_u64();

            spawn(async move {
                if let Err(err) = handle(conn, up, down, id << 1).await {
                    eprintln!("{} from {}", err, peer_addr)
                }
            });
        }
    }
}

async fn handle(
    conn: IncomingConnection<()>,
    up: SocketAddr,
    down: SocketAddr,
    id: u64,
) -> Result<(), Error> {
    let (conn, addr) = handle_socks5(conn).await?;

    let (res_up, res_down) = join!(
        dial_upstream(up, addr.clone()),
        dial_upstream(down, addr.clone())
    );
    let ((r_up, mut up), (r_down, mut down)) = (res_up?, res_down?);

    let reply = if r_up != Reply::Succeeded {
        r_up
    } else if r_down != Reply::Succeeded {
        r_down
    } else {
        Reply::Succeeded
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

        return Err(other_error("Unsuccessful socks5 reply"));
    }

    let (mut up_c, mut down_c) = split(conn);

    spawn(async move {
        if let Err(_) = down.write_u64(id | 1).await {
            return;
        };
        let _ = copy(&mut down, &mut down_c).await;
        let _ = down_c.shutdown().await;
    });

    up.write_u64(id).await?;
    let _ = copy(&mut up_c, &mut up).await;

    Ok(())
}

async fn dial_upstream(upstream: SocketAddr, addr: Address) -> Result<(Reply, TcpStream), Error> {
    let mut upstream = TcpStream::connect(upstream).await?;

    {
        let req = handshake::Request::new(vec![handshake::Method::NONE]);
        req.write_to(&mut upstream).await?;

        let resp = handshake::Response::read_from(&mut upstream).await?;
        if resp.method != handshake::Method::NONE {
            let _ = upstream.shutdown().await;
            return Err(other_error("No acceptable auth method"));
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
