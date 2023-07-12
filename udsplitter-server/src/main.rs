#![feature(io_error_other)]

use std::{
    collections::HashMap,
    env::args,
    fs::File,
    io::Error as IoError,
    sync::Arc,
    time::{Duration, Instant},
};

use socks5_proto::{Address, Error, Reply};
use socks5_server::{auth::NoAuth, IncomingConnection, Server as Socks5Server};
use tokio::{
    io::{copy, split, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    spawn,
    sync::Mutex,
    time::{sleep, timeout},
};

use serde::Deserialize;

use common::handle_socks5;

#[derive(Deserialize)]
struct Config {
    listen: String,
}

enum RW {
    R(ReadHalf<TcpStream>),
    W(WriteHalf<TcpStream>),
}

type ConnMap = HashMap<u64, (Instant, Arc<Mutex<Option<RW>>>)>;

const TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() {
    let config: Config =
        serde_json::from_reader(File::open(args().nth(1).unwrap()).unwrap()).unwrap();

    let listener = TcpListener::bind(&config.listen).await.unwrap();
    let server = Socks5Server::from((listener, Arc::new(NoAuth) as Arc<_>));

    let conn_map = Arc::new(Mutex::new(ConnMap::new()));

    {
        let conn_map = conn_map.clone();
        // garbage collection
        spawn(async move {
            loop {
                sleep(Duration::from_secs(30)).await;

                let now = Instant::now();
                let mut conn_map = conn_map.lock().await;
                conn_map.retain(|_, (t, _)| now - t.to_owned() < TIMEOUT);
            }
        });
    }

    loop {
        if let Ok((conn, _)) = server.accept().await {
            let conn_map = conn_map.clone();

            spawn(async move {
                eprintln!("Connection from {}", conn.peer_addr().unwrap());
                if let Err(err) = handle(conn, conn_map).await {
                    eprintln!("{}", err)
                }
            });
        }
    }
}

async fn handle(conn: IncomingConnection<()>, conn_map: Arc<Mutex<ConnMap>>) -> Result<(), Error> {
    let (conn, addr) = handle_socks5(conn).await?;

    let mut conn = match conn.reply(Reply::Succeeded, Address::unspecified()).await {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err.into());
        }
    };

    let (mut r, mut w) = conn.split();

    let mut id = timeout(TIMEOUT, r.read_u64())
        .await
        .map_err(|_| IoError::other("Timeout while reading connection id"))?
        .unwrap();

    let is_down = (id & 1) != 0;
    id >>= 1;

    {
        let mut conn_map = conn_map.lock().await;

        if let Some((t, rw)) = conn_map.remove(&id) {
            drop(conn_map);

            eprintln!(
                "{} arrived {}ms later",
                if is_down { "Down stream" } else { "Up stream" },
                (Instant::now() - t).as_millis()
            );

            let mut rw = rw.lock().await;

            let mismatched = Err(IoError::other("Mismatched connection type").into());
            match rw.as_mut().unwrap() {
                RW::R(down_r) => {
                    if !is_down {
                        return mismatched;
                    }

                    let _ = copy(down_r, &mut w).await;
                }
                RW::W(up_r) => {
                    if is_down {
                        return mismatched;
                    }

                    let _ = copy(&mut r, up_r).await;
                }
            }
        } else {
            let conn_rs = Arc::new(Mutex::new(None));
            let conn_ra = conn_rs.clone();
            let mut conn_rl = conn_ra.lock().await;

            conn_map.insert(id, (Instant::now(), conn_rs));
            drop(conn_map);

            let conn_r = match addr {
                Address::SocketAddress(addr) => TcpStream::connect(addr).await,
                Address::DomainAddress(host, port) => {
                    TcpStream::connect((String::from_utf8_lossy(&host).as_ref(), port)).await
                }
            };

            let (mut down_r, mut up_r) = match conn_r {
                Ok(conn) => split(conn),
                Err(err) => {
                    let _ = w.shutdown().await;

                    return Err(err.into());
                }
            };

            if is_down {
                let _ = conn_rl.insert(RW::W(up_r));
                drop(conn_rl);
                drop(conn_ra);

                let _ = copy(&mut down_r, &mut w).await;
            } else {
                let _ = conn_rl.insert(RW::R(down_r));
                drop(conn_rl);
                drop(conn_ra);

                let _ = copy(&mut r, &mut up_r).await;
            }
        }
    }

    Ok(())
}
