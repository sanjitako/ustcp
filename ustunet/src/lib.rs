#[macro_use]
extern crate log;
mod dispatch;
mod listener;
mod sockets;
pub mod stream;
mod time;
pub(crate) mod util;
use snafu::Snafu;

pub use listener::TcpListener;
pub(crate) use tokio::sync::mpsc;
pub(crate) use trilock::TriLock as SocketLock;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("IO error {}", source))]
    Io { source: std::io::Error },
}