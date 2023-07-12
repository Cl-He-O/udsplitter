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
    io::{copy, split, AsyncReadExt, AsyncWriteExt},
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

type ConnMap = HashMap<u64, (Instant, TcpStream)>;

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
                if let Err(err) = handle(conn, conn_map).await {
                    eprintln!("{}", err)
                }
            });
        }
    }
}

async fn handle(conn: IncomingConnection<()>, conn_map: Arc<Mutex<ConnMap>>) -> Result<(), Error> {
    let (conn, addr) = handle_socks5(conn).await?;

    let conn = match conn.reply(Reply::Succeeded, Address::unspecified()).await {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err.into());
        }
    };

    let mut conn: TcpStream = conn.into();

    let mut id = timeout(TIMEOUT, conn.read_u64())
        .await
        .map_err(|_| IoError::other("Timeout while reading connection id"))?
        .unwrap();
    
    let is_down = (id & 1) != 0;
    id >>= 1;

    {
        let mut conn_map = conn_map.lock().await;

        if let Some((_, conn_early)) = conn_map.remove(&id) {
            drop(conn_map);

            let conn_early = conn_early;

            let conn_r = match addr {
                Address::SocketAddress(addr) => TcpStream::connect(addr).await,
                Address::DomainAddress(host, port) => {
                    TcpStream::connect((String::from_utf8_lossy(&host).as_ref(), port)).await
                }
            };

            let conn_r = match conn_r {
                Ok(conn) => conn,
                Err(err) => {
                    let _ = conn.shutdown().await;

                    return Err(err.into());
                }
            };

            let (mut up, mut down) = if is_down {
                (conn_early, conn)
            } else {
                (conn, conn_early)
            };

            let (mut down_r, mut up_r) = split(conn_r);

            spawn(async move {
                let _ = copy(&mut down_r, &mut down).await;
            });

            let _ = copy(&mut up, &mut up_r).await;
        } else {
            let conn = conn;

            conn_map.insert(id, (Instant::now(), conn));
            drop(conn_map);
        }
    }

    Ok(())
}
