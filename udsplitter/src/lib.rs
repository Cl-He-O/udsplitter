#[cfg(test)]
mod test {
    use std::{
        hash::Hasher,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use rand::RngCore;
    use socks5_proto::*;
    use tokio::{
        io::{copy, AsyncReadExt, AsyncWrite, AsyncWriteExt},
        net::TcpListener,
        spawn,
        sync::Barrier,
        task::JoinSet,
    };

    struct HasherConn(std::hash::DefaultHasher);

    impl AsyncWrite for HasherConn {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, std::io::Error>> {
            self.0.write(buf);
            Poll::Ready(Ok(buf.len()))
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
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn stress() {
        const N_CONN: usize = 100;
        const SIZE_DATA: usize = 1000000;

        let mut data = vec![0; SIZE_DATA];
        rand::thread_rng().fill_bytes(&mut data);
        let datahash = {
            let mut hasher = std::hash::DefaultHasher::new();
            hasher.write(&data);
            hasher.finish()
        };
        let data = Arc::new(data);

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

        spawn(udsplitter_client::start(utils::config_from_path(
            "../example/client.json",
        )));
        spawn(udsplitter_server::start(utils::config_from_path(
            "../example/server.json",
        )));

        let mut tasks = JoinSet::new();
        for _ in 0..N_CONN {
            let data = data.clone();
            let barrier = barrier.clone();

            tasks.spawn(async move {
                let (reply, mut conn) = udsplitter_client::dial_upstream(
                    ([127, 0, 0, 2], 1080).into(),
                    Address::SocketAddress(([127, 0, 0, 2], 1082).into()),
                )
                .await
                .unwrap();

                assert_eq!(reply, Reply::Succeeded);

                conn.write_all(&data).await.unwrap();

                let mut hasher = HasherConn(std::hash::DefaultHasher::new());
                copy(&mut conn.take(SIZE_DATA as u64), &mut hasher)
                    .await
                    .unwrap();
                let rdatahash = hasher.0.finish();

                assert_eq!(datahash, rdatahash);

                barrier.wait().await;
            });
        }

        while let Some(Ok(_)) = tasks.join_next().await {}
    }
}
