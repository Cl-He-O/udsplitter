use std::{
    collections::HashMap,
    env::args,
    fs::File,
    sync::Arc,
    time::{Duration, Instant},
};

use socks5_proto::{Address, Error, Reply};
use socks5_server::{
    auth::NoAuth,
    connection::{connect::state::Ready, state::NeedAuthenticate},
    Connect, IncomingConnection, Server as Socks5Server,
};
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
    #[serde(skip)]
    timeout: Duration,
}

impl Config {
    fn default_timeout_ms() -> u64 {
        1000
    }
}

enum ConnHalf {
    R(ReadHalf<TcpStream>),
    W(WriteHalf<TcpStream>),
}

type ConnMap = HashMap<u64, (Instant, Arc<Mutex<Option<ConnHalf>>>)>;

#[tokio::main]
async fn main() {
    let mut config: Config =
        serde_json::from_reader(File::open(args().nth(1).unwrap()).unwrap()).unwrap();
    config.timeout = Duration::from_millis(config.timeout_ms);

    let listener = TcpListener::bind(&config.listen).await.unwrap();
    let server = Socks5Server::new(listener, Arc::new(NoAuth) as Arc<_>);

    let conn_map = Arc::new(Mutex::new(ConnMap::new()));

    {
        let conn_map = conn_map.clone();

        spawn(async move {
            loop {
                // session will live for 1.5*timeout at most
                sleep(config.timeout / 2).await;

                let mut conn_map = conn_map.lock().await;
                let until = Instant::now() - config.timeout;

                conn_map.retain(|_, &mut (v, _)|until < v);
                eprintln!("conn_map.len()=={}", conn_map.len());
            }
        });
    }

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
    conn: IncomingConnection<(), NeedAuthenticate>,
    conn_map: Arc<Mutex<ConnMap>>,
    dur_timeout: Duration,
) -> Result<(), Error> {
    let (conn, addr) = handle_socks5(conn).await?;

    let conn = match conn.reply(Reply::Succeeded, Address::unspecified()).await {
        Ok(conn) => conn,
        Err((err, mut conn)) => {
            let _ = conn.shutdown().await;
            return Err(err.into());
        }
    };

    let (mut r, mut w) = split(conn);

    let mut id = timeout(dur_timeout, r.read_u64())
        .await
        .map_err(|_| other_error("Timeout while reading connection id"))?
        .unwrap();

    let is_down = (id & 1) != 0;
    id >>= 1;

    let mut conn_map_l = conn_map.lock().await;

    if let Some((t, conn_half)) = conn_map_l.remove(&id) {
        // complete session
        drop(conn_map_l);

        eprintln!(
            "{} stream arrived {}ms later",
            if is_down { "Down" } else { "Up" },
            (Instant::now() - t).as_millis()
        );

        let mut conn_half = conn_half.lock().await;

        let mismatched = Err(other_error("Mismatched connection type"));

        // start copying
        match conn_half.as_mut().unwrap() {
            ConnHalf::R(down) => {
                if !is_down {
                    return mismatched;
                }

                let _ = copy(down, &mut w).await;
            }
            ConnHalf::W(up) => {
                if is_down {
                    return mismatched;
                }

                let _ = copy(&mut r, up).await;
            }
        }
    } else {
        // new session
        let conn_remote_arc = Arc::new(Mutex::new(None)); // remote connection
        let mut conn_remote_lock = conn_remote_arc.lock().await; // lock here prevents conn_half.is_none() case

        {
            let conn_remote_arc = conn_remote_arc.clone();
            conn_map_l.insert(id, (Instant::now(), conn_remote_arc));
            drop(conn_map_l);
        }

        let (mut down, mut up) = connect_remote(addr.clone(), &mut w).await?;

        // start copying
        let _ = if is_down {
            let _ = conn_remote_lock.insert(ConnHalf::W(up));
            drop(conn_remote_lock);
            drop(conn_remote_arc);

            copy(&mut down, &mut w).await
        } else {
            let _ = conn_remote_lock.insert(ConnHalf::R(down));
            drop(conn_remote_lock);
            drop(conn_remote_arc);

            copy(&mut r, &mut up).await
        };
    }

    Ok(())
}

async fn connect_remote(
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
