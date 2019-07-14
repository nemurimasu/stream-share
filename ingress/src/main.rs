use bytes::{Bytes, BytesMut};
use futures::prelude::*;
use lazy_static::lazy_static;
use log::{error, info};
use net2::TcpBuilder;
use std::borrow::Cow;
use std::env;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::prelude::*;
use tokio::reactor::Handle;

const ICE_ROUTES: &[&[u8]] = &[b"GET /output", b"PUT /input", b"SOURCE "];
const ROUTE_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_BACKLOG: i32 = 1024;
const ROUTE_BUFFER_LEN: usize = 512;

lazy_static! {
    static ref ICE_ADDR: SocketAddr = {
        env::var("ICE_ADDR")
            .map(Cow::Owned)
            .or_else(|e| match e {
                env::VarError::NotPresent => Ok(Cow::Borrowed("localhost:8000")),
                e => Err(e),
            })
            .unwrap()
            .parse()
            .unwrap()
    };
    static ref DEFAULT_ADDR: SocketAddr = {
        env::var("DEFAULT_ADDR")
            .map(Cow::Owned)
            .or_else(|e| match e {
                env::VarError::NotPresent => Ok(Cow::Borrowed("localhost:5000")),
                e => Err(e),
            })
            .unwrap()
            .parse()
            .unwrap()
    };
}

fn process_socket(
    socket: TcpStream,
    ice_addr: &'static SocketAddr,
    default_addr: &'static SocketAddr,
) -> impl Future<Item = (), Error = tokio::timer::timeout::Error<std::io::Error>> {
    Router::new(socket, ice_addr, default_addr)
        .timeout(ROUTE_TIMEOUT)
        .and_then(|(addr, client, buf)| {
            TcpStream::connect(addr)
                .and_then(|server| {
                    let (client_reader, client_writer) = client.split();
                    let (server_reader, server_writer) = server.split();
                    tokio::spawn(
                        tokio::io::copy(server_reader, client_writer)
                            .map(|_| ())
                            .map_err(|e| error!("failed to forward to client: {:?}", e)),
                    );
                    tokio::io::write_all(server_writer, buf)
                        .and_then(|(server_writer, _)| {
                            tokio::io::copy(client_reader, server_writer)
                        })
                        .map(|_| ())
                })
                .map_err(|e| tokio::timer::timeout::Error::inner(e))
        })
}

struct Router<'a, C> {
    client: Option<C>,
    buffer: Option<BytesMut>,
    ice: &'a SocketAddr,
    default: &'a SocketAddr,
}

impl<'a, C> Router<'a, C> {
    pub fn new(client: C, ice: &'a SocketAddr, default: &'a SocketAddr) -> Self {
        Self {
            client: Some(client),
            buffer: Some(BytesMut::with_capacity(ROUTE_BUFFER_LEN)),
            ice,
            default,
        }
    }
}

impl<'a, C> Future for Router<'a, C>
where
    C: AsyncRead,
{
    type Item = (&'a SocketAddr, C, Bytes);
    type Error = std::io::Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        let buffer = self.buffer.as_mut().unwrap();
        match self.client.as_mut().unwrap().read_buf(buffer) {
            Ok(Async::Ready(0)) => Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            Ok(Async::Ready(_)) => {
                if ICE_ROUTES
                    .iter()
                    .cloned()
                    .any(|route| route.len() <= buffer.len() && route == &buffer[0..route.len()])
                {
                    Ok(Async::Ready((
                        self.ice,
                        self.client.take().unwrap(),
                        self.buffer.take().unwrap().freeze(),
                    )))
                } else if buffer.len() > 0
                    && ICE_ROUTES.iter().cloned().all(|route| {
                        route.len() <= buffer.len() || &route[0..buffer.len()] != &buffer[..]
                    })
                {
                    Ok(Async::Ready((
                        self.default,
                        self.client.take().unwrap(),
                        self.buffer.take().unwrap().freeze(),
                    )))
                } else {
                    Ok(Async::NotReady)
                }
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(error) => Err(error),
        }
    }
}

fn main() {
    env_logger::from_env(env_logger::Env::default().default_filter_or("debug")).init();

    let addr = env::var("INGRESS_BIND")
        .map(Cow::Owned)
        .or_else(|e| match e {
            env::VarError::NotPresent => Ok(Cow::Borrowed("[::]:80")),
            e => Err(e),
        })
        .unwrap()
        .parse()
        .unwrap();

    let listener = match addr {
        SocketAddr::V6(addr) => TcpBuilder::new_v6()
            .unwrap()
            .only_v6(false)
            .unwrap()
            .bind(addr)
            .unwrap()
            .listen(ACCEPT_BACKLOG),
        SocketAddr::V4(addr) => TcpBuilder::new_v4()
            .unwrap()
            .bind(addr)
            .unwrap()
            .listen(ACCEPT_BACKLOG),
    }
    .unwrap();

    info!("Listening on http://{}/", listener.local_addr().unwrap());
    let listener = tokio::net::TcpListener::from_std(listener, &Handle::default()).unwrap();
    tokio::run(
        listener
            .incoming()
            .map_err(|e| error!("failed to accept socket: {:?}", e))
            .for_each(|socket| {
                tokio::spawn(
                    process_socket(socket, &ICE_ADDR, &DEFAULT_ADDR)
                        .map_err(|e| error!("failed to route socket: {:?}", e)),
                )
            }),
    );
}
