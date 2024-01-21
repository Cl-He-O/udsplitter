#[cfg(test)]
mod test {
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use socks5_proto::*;
    use tokio::{
        io::{copy, split, AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
        net::TcpListener,
        spawn,
        sync::Barrier,
        task::JoinSet,
    };

    struct VerifyConn;

    const PATERN: &[u8] = &[0; 8192];

    impl AsyncWrite for VerifyConn {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, std::io::Error>> {
            let read_len = buf.len().min(PATERN.len());

            assert_eq!(&buf[..read_len], &PATERN[..read_len]);

            Poll::Ready(Ok(read_len))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct PaternConn(usize);

    impl AsyncRead for PaternConn {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let fill_len = buf.remaining().min(self.0).min(PATERN.len());
            self.0 -= fill_len;

            buf.put_slice(&PATERN[..fill_len]);

            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn stress() {
        const N_CONN: usize = 100;
        const DATA_SIZE: usize = 10000000;

        let barrier = Arc::new(Barrier::new(N_CONN + 1));

        {
            let barrier = barrier.clone();
            let listen = TcpListener::bind("127.0.0.2:1082").await.unwrap();
            spawn(async move {
                for _ in 0..N_CONN {
                    let (mut conn, _) = listen.accept().await.unwrap();
                    spawn(async move {
                        let (mut r, mut w) = conn.split();
                        copy(&mut r, &mut w).await.unwrap();
                    });
                }

                barrier.wait().await;
            });
        }

        spawn(udsplitter_client::start(utils::config_from_str(
            r#"
        {
            "listen": "127.0.0.2:1080",
            "up": "127.0.0.2:1081",
            "down": "127.0.0.2:1081"
        }
        "#,
        )));
        spawn(udsplitter_server::start(utils::config_from_str(
            r#"
        {
            "listen": "127.0.0.2:1081",
            "timeout_ms": 5000,
            "timeout_id_ms": 1000
        }
        "#,
        )));

        let mut tasks = JoinSet::new();
        for _ in 0..N_CONN {
            let barrier = barrier.clone();

            tasks.spawn(async move {
                let (reply, conn) = udsplitter_client::dial_upstream(
                    ([127, 0, 0, 2], 1080).into(),
                    Address::SocketAddress(([127, 0, 0, 2], 1082).into()),
                )
                .await
                .unwrap();

                assert_eq!(reply, Reply::Succeeded);

                let (r, mut w) = split(conn);
                spawn(async move {
                    let mut paternconn = PaternConn(DATA_SIZE);
                    copy(&mut paternconn, &mut w).await.unwrap();
                });

                let mut verifyconn = VerifyConn;
                copy(&mut r.take(DATA_SIZE as u64), &mut verifyconn)
                    .await
                    .unwrap();

                barrier.wait().await;
            });
        }

        while let Some(Ok(_)) = tasks.join_next().await {}
    }
}
