#[macro_use]
extern crate log;
use argh::FromArgs;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use ustunet;
use ustunet::TcpListener;

// #[derive(FromArgs)]
// /// Echoing server on every address.
// struct EchoUp {
//     /// tun device owned by current user
//     #[argh(option)]
//     tun: String,
// }

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    // let up: EchoUp = argh::from_env();
    info!("start");
    let mut echo_server = TcpListener::bind("tuna").unwrap();
    while let Some(mut socket) = echo_server.next().await {
        debug!("accepted new tcp stream");
        let mut buf = vec![0u8; 1024];
        tokio::spawn(async move {
            loop {
                let n = socket.read(&mut buf).await.expect("read");
                if n == 0 {
                    info!("stream closed");
                    break;
                }
                let content = &buf[..n];
                eprintln!("read {:?} bytes: {:?}", n,content);
                eprintln!("content : {:?}",String::from_utf8_lossy(content));
                let n = socket.write(content).await.expect("write failed");
                println!("Written {} bytes", n);
            }
            eprintln!("connection ended");
        });
    }
}
