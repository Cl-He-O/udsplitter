use std::{
    collections::HashMap,
    env::args,
    fs::File,
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

use common::{handle_socks5, other_error};

#[derive(Deserialize)]
struct Config {
    listen: String,

    #[serde(default = "Config::default_timeout_ms")]
    timeout_ms: u64,
}

impl Config {
    fn default_timeout_ms() -> u64 {
        1000
    }
}

enum RW {
    R(ReadHalf<TcpStream>),
    W(WriteHalf<TcpStream>),
}

type ConnMap = HashMap<u64, (Instant, Arc<Mutex<Option<RW>>>)>;

#[tokio::main]
async fn main() {
    let config: Config =
        serde_json::from_reader(File::open(args().nth(1).unwrap()).unwrap()).unwrap();

    let listener = TcpListener::bind(&config.listen).await.unwrap();
    let server = Socks5Server::from((listener, Arc::new(NoAuth) as Arc<_>));

    let conn_map = Arc::new(Mutex::new(ConnMap::new()));

    loop {
        if let Ok((conn, _)) = server.accept().await {
            let conn_map = conn_map.clone();

            spawn(async move {
                let peer_addr = conn.peer_addr().unwrap();
                eprintln!("Connection from {}", peer_addr);
                if let Err(err) =
                    handle(conn, conn_map, Duration::from_millis(config.timeout_ms)).await
                {
                    eprintln!("{} from {}", err, peer_addr);
                }
            });
        }
    }
}

async fn handle(
    conn: IncomingConnection<()>,
    conn_map: Arc<Mutex<ConnMap>>,
    ttimeout: Duration,
) -> Result<(), Error> {
    let peer_addr = conn.peer_addr().unwrap();

    let (conn, addr) = handle_socks5(conn).await?;

    let conn = match conn.reply(Reply::Succeeded, Address::unspecified()).await {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err.into());
        }
    };

    let (mut r, mut w) = split(conn);

    let mut id = timeout(ttimeout, r.read_u64())
        .await
        .map_err(|_| other_error("Timeout while reading connection id"))?
        .unwrap();

    let is_down = (id & 1) != 0;
    id >>= 1;

    {
        let mut conn_map_l = conn_map.lock().await;

        if let Some((t, rw)) = conn_map_l.remove(&id) {
            drop(conn_map_l);

            eprintln!(
                "{} stream arrived {}ms later",
                if is_down { "Down" } else { "Up" },
                (Instant::now() - t).as_millis()
            );

            let mut rw = rw.lock().await;

            let mismatched = Err(other_error("Mismatched connection type"));

            if let Some(rw) = rw.as_mut() {
                match rw {
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
            }
        } else {
            let conn_rs = Arc::new(Mutex::new(None));
            let conn_ra = conn_rs.clone();
            let mut conn_rl = conn_ra.lock().await;

            conn_map_l.insert(id, (Instant::now(), conn_rs));
            drop(conn_map_l);

            let conn_r = match addr.clone() {
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

            spawn(async move {
                sleep(ttimeout).await;

                let mut conn_map = conn_map.lock().await;
                let conn = conn_map.remove(&id);
                drop(conn_map);

                if conn.is_some() {
                    eprintln!("Connection timeout from {}", peer_addr);
                };
            });

            let _ = if is_down {
                let _ = conn_rl.insert(RW::W(up_r));
                drop(conn_rl);
                drop(conn_ra);

                copy(&mut down_r, &mut w).await
            } else {
                let _ = conn_rl.insert(RW::R(down_r));
                drop(conn_rl);
                drop(conn_ra);

                copy(&mut r, &mut up_r).await
            };
        }
    }

    Ok(())
}
