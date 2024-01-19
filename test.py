import socket

from socks import socksocket, SOCKS5
from secrets import token_bytes
from xxhash import xxh3_128
from subprocess import run, DEVNULL
from threading import Thread
from time import sleep

DATA_SIZE = 10000
N_CONNS = 1000

client_t = Thread(
    target=run,
    kwargs={
        "args": ["target/release/udsplitter-client", "example/client.json"],
        "stderr": DEVNULL,
    },
)
client_t.daemon = True
client_t.start()

server_t = Thread(
    target=run,
    kwargs={
        "args": ["target/release/udsplitter-server", "example/server.json"],
        "stderr": DEVNULL,
    },
)
server_t.daemon = True
server_t.start()


l = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
l.bind(("127.0.0.2", 1082))
l.listen()


def recvall(conn, n):
    data = bytearray()
    while len(data) < n:
        r = conn.recv(n - len(data))
        if not r:
            return None
        data.extend(r)
    return data


def listen():
    for _ in range(N_CONNS):
        (conn, addr) = l.accept()

        def handle(conn: socket.socket):
            conn.recv_into
            conn.sendall(recvall(conn, DATA_SIZE))
            sleep(3)
            conn.close()

        Thread(target=handle, args=[conn]).start()


listen_t = Thread(target=listen)
listen_t.daemon = True
listen_t.start()

data = token_bytes(DATA_SIZE)


def connect_socks5(i):
    s = socksocket()
    s.set_proxy(SOCKS5, "127.0.0.2", 1080)

    s.connect(("127.0.0.2", 1082))

    s.sendall(data)
    data_back = recvall(s, DATA_SIZE)

    if xxh3_128(data_back).digest() != xxh3_128(data).digest():
        print(f"mismatch data {i}, {len(data_back)}/{len(data)}")

    sleep(3)
    s.close()


sleep(0.1)

for i in range(N_CONNS):
    connect_t = Thread(target=connect_socks5, args=[i])
    connect_t.daemon = True
    connect_t.start()

listen_t.join()
print("ok")
