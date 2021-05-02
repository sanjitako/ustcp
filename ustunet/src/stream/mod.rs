//! TcpStream and private structures.
use super::SocketLock;
use crate::dispatch::poll_queue::QueueUpdater;
use crate::dispatch::{Close, CloseSender, HalfCloseSender, SocketHandle};
use crate::sockets::AddrPair;
use crate::stream::internal::{TcpConnection, UdpConnection};
use futures::future::poll_fn;
use futures::io::Error;
use futures::ready;
use futures::task::Poll;
use smoltcp::socket::TcpSocket;
use smoltcp::socket::TcpState;
use smoltcp::socket::UdpSocket;
use smoltcp::socket::UdpPacketMetadata;
use std::borrow::BorrowMut;
use std::fmt;
use std::fmt::Formatter;
use std::io;
use std::net::SocketAddr;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync as std_sync;
use std::sync::Arc;
use std::task;
use std::task::{Context, Waker};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) type ReadinessState = Arc<std_sync::Mutex<SharedState>>;
pub(crate) type WriteReadiness = Arc<std_sync::Mutex<SharedState>>;

pub(crate) mod internal;

type Tcp = TcpSocket<'static>;
type Udp = UdpSocket<'static>;
type TcpLock = SocketLock<TcpInner>;
type UdpLock = SocketLock<UdpInner>;


pub(crate) struct TcpInner {
    pub(crate) tcp: Tcp,
    /// Whether the connection is in queue to be dispatched.
    /// If true, data in the tx buffer will be sent out.
    /// If false, the dispatch queue should be notified
    /// after writing to the tx buffer.
    polling_active: bool,
}
pub(crate) struct UdpInner {
    pub(crate) udp: Udp,
    /// Whether the connection is in queue to be dispatched.
    /// If true, data in the tx buffer will be sent out.
    /// If false, the dispatch queue should be notified
    /// after writing to the tx buffer.
    polling_active: bool,
}


pub struct TcpStream {
    writer: TcpWriteHalf,
    reader: TcpReadHalf,
    /// Local and peer address.
    // Immutable.
    addr: AddrPair,
}

pub struct UdpStream{
    writer: UdpWriteHalf,
    reader: UdpReadHalf,
    /// Local and peer address.
    // Immutable.
    addr: AddrPair,
}

pub struct TcpReadHalf {
    mutex: TcpLock,
    shared_state: ReadinessState,
    eof_reached: bool,
    /// Send a message when getting dropped.
    shutdown_notifier: HalfCloseSender,
}

pub struct TcpWriteHalf {
    mutex: TcpLock,
    shared_state: ReadinessState,
    handle: SocketHandle,
    notifier: QueueUpdater,
    shutdown_notifier: HalfCloseSender,
}

pub struct UdpReadHalf{
    mutex: UdpLock,
    shared_state: ReadinessState,
    eof_reached: bool,
    /// Send a message when getting dropped.
    shutdown_notifier: HalfCloseSender,
}

pub struct UdpWriteHalf {
    mutex: UdpLock,
    shared_state: ReadinessState,
    handle: SocketHandle,
    notifier: QueueUpdater,
    shutdown_notifier: HalfCloseSender,
}



#[derive(Debug)]
pub(crate) struct SharedState {
    pub waker: Option<Waker>,
}

impl SharedState {
    pub(crate) fn wake_once(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

impl TcpStream {
    /// Returns a TcpStream and a struct containing references used
    /// internally to move data between buffers and network interfaces.
    pub(crate) fn new(
        tcp: Tcp,
        poll_queue: QueueUpdater,
        addr: AddrPair,
        shutdown_builder: &CloseSender,
    ) -> (TcpStream, TcpConnection) {
        let inner = TcpInner {
            tcp,
            polling_active: false,
        };
        let tcp_locks = SocketLock::new(inner);
        let (reader, set_ready) = TcpReadHalf::new(
            tcp_locks.0,
            shutdown_builder.build(addr.clone(), Close::Read),
        );
        let (writer, write_readiness) = TcpWriteHalf::new(
            tcp_locks.1,
            poll_queue,
            addr.clone(),
            shutdown_builder.build(addr.clone(), Close::Write),
        );
        let tcp = TcpStream {
            reader,
            writer,
            addr: addr.clone(),
        };
        let connection = TcpConnection::new(tcp_locks.2, addr, set_ready, write_readiness);
        (tcp, connection)
    }

    pub fn reader(&mut self) -> &mut TcpReadHalf {
        &mut self.reader
    }
    pub fn writer(&mut self) -> &mut TcpWriteHalf {
        &mut self.writer
    }
    /// Returns the local address that this TcpStream is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr.local
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> SocketAddr {
        self.addr.peer
    }

    pub fn split(self) -> (TcpReadHalf, TcpWriteHalf) {
        (self.reader, self.writer)
    }

    /// Close the transmit half of the full-duplex connection.
    pub async fn close(&mut self) {
        self.writer.close().await;
    }
}

//TODO new for UdpStream
impl UdpStream {
    pub(crate) fn new(
        udp: Udp,
        poll_queue: QueueUpdater,
        addr: AddrPair,
        shutdown_builder: &CloseSender,
    ) -> (UdpStream, UdpConnection) {
        let inner = UdpInner {
            udp,
            polling_active: false,
        };
        let udp_locks = SocketLock::new(inner);
        let (reader, set_ready) = UdpReadHalf::new(
            udp_locks.0,
            shutdown_builder.build(addr.clone(), Close::Read),
        );
        let (writer, write_readiness) = UdpWriteHalf::new(
            udp_locks.1,
            poll_queue,
            addr.clone(),
            shutdown_builder.build(addr.clone(), Close::Write),
        );
        let udp = UdpStream {
            reader,
            writer,
            addr: addr.clone(),
        };
        let connection = UdpConnection::new(udp_locks.2, addr, set_ready, write_readiness);
        (udp, connection)
    }

    pub fn reader(&mut self) -> &mut UdpReadHalf {
        &mut self.reader
    }
    pub fn writer(&mut self) -> &mut UdpWriteHalf {
        &mut self.writer
    }
    /// Returns the local address that this TcpStream is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr.local
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> SocketAddr {
        self.addr.peer
    }

    pub fn split(self) -> (UdpReadHalf, UdpWriteHalf) {
        (self.reader, self.writer)
    }

    /// Close the transmit half of the full-duplex connection.
    pub async fn close(&mut self) {
        self.writer.close().await;
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let b = buf.initialize_unfilled();
        self.reader
            .poll_read_inner(cx, b)
            .map(|o| o.map(|n| buf.advance(n)))
    }
}

impl futures::io::AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.reader.poll_read_inner(cx, buf)
    }
}

// // TODO AsyncRead for UdpStream
// impl AsyncRead for UdpStream{
//     fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
//         let b = buf.initialize_unfilled();
//         self.reader
//             .poll_read_inner(cx,b)
//             .map(|o| o.map(|n| buf.advance(n)))
//     }
// }
// // TODO futures::io::AsyncRead for UdpStream
// impl futures::io::AsyncRead for UdpStream {
//     fn poll_read(
//         mut self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//         buf: &mut [u8],
//     ) -> Poll<io::Result<usize>> {
//         self.reader.poll_read_inner(cx, buf)
//     }
// }

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        self.writer.poll_write_inner(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.writer.poll_close_inner(cx).map(|_| Ok(()))
    }
}

impl futures::io::AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        self.writer.poll_write_inner(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.writer.poll_close_inner(cx).map(|_| Ok(()))
    }
}

// TODO AsyncWrite for UdpStream

// TODO futures::io::AsyncWrite for UdpStream

impl TcpReadHalf {
    fn new(socket: TcpLock, shutdown_notifier: HalfCloseSender) -> (Self, ReadinessState) {
        let state = SharedState { waker: None };
        let shared_state = Arc::new(std_sync::Mutex::new(state));
        let s = TcpReadHalf {
            mutex: socket,
            shared_state: shared_state.clone(),
            eof_reached: false,
            shutdown_notifier,
        };
        (s, shared_state)
    }
}

impl TcpWriteHalf {
    fn new(
        socket: TcpLock,
        notifier: QueueUpdater,
        handle: SocketHandle,
        shutdown_notifier: HalfCloseSender,
    ) -> (Self, WriteReadiness) {
        let shared_state = Arc::new(std_sync::Mutex::new(SharedState { waker: None }));
        let s = Self {
            mutex: socket,
            shared_state: shared_state.clone(),
            notifier,
            handle,
            shutdown_notifier,
        };
        (s, shared_state)
    }

    /// Close the transmit half of the full-duplex connection.
    pub async fn close(&mut self) {
        poll_fn(|cx| self.poll_close_inner(cx)).await
    }
    fn poll_close_inner(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let mut guard = ready!(self.mutex.poll_lock(cx));
        let TcpInner {
            tcp,
            polling_active,
        } = guard.deref_mut();
        tcp.close();
        if !*polling_active {
            self.notifier.send(self.handle.clone(), tcp.poll_at());
        };
        Poll::Ready(())
    }
}

// TODO new for UdpReadHalf
impl UdpReadHalf {
    fn new(socket: UdpLock, shutdown_notifier: HalfCloseSender) -> (Self, ReadinessState) {
        let state = SharedState { waker: None };
        let shared_state = Arc::new(std_sync::Mutex::new(state));
        let s = UdpReadHalf {
            mutex: socket,
            shared_state: shared_state.clone(),
            eof_reached: false,
            shutdown_notifier,
        };
        (s, shared_state)
    }
}
// TODO new for UdpWriteHalf
impl UdpWriteHalf {
    fn new(
        socket: UdpLock,
        notifier: QueueUpdater,
        handle: SocketHandle,
        shutdown_notifier: HalfCloseSender,
    ) -> (Self, WriteReadiness) {
        let shared_state = Arc::new(std_sync::Mutex::new(SharedState { waker: None }));
        let s = Self {
            mutex: socket,
            shared_state: shared_state.clone(),
            notifier,
            handle,
            shutdown_notifier,
        };
        (s, shared_state)
    }
    /// Close the transmit half of the full-duplex connection.
    pub async fn close(&mut self) {
        poll_fn(|cx| self.poll_close_inner(cx)).await
    }

    fn poll_close_inner(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let mut guard = ready!(self.mutex.poll_lock(cx));
        let UdpInner {
            udp,
            polling_active,
        } = guard.deref_mut();
        if !*polling_active {
            self.notifier.send(self.handle.clone(), udp.poll_at());
        };
        Poll::Ready(())
    }
}

impl AsyncRead for TcpReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let b = buf.initialize_unfilled();
        self.as_mut()
            .poll_read_inner(cx, b)
            .map(|o| o.map(|n| buf.advance(n)))
    }
}

impl futures::io::AsyncRead for TcpReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.as_mut().poll_read_inner(cx, buf)
    }
}

impl TcpReadHalf {
    fn poll_read_inner(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let Self {
            ref mut mutex,
            ref mut shared_state,
            ref mut eof_reached,
            ..
        } = self;
        let l = mutex.poll_lock(cx);
        let mut guard = ready!(l);
        let tcp = &mut guard.borrow_mut().tcp;
        if tcp.can_recv() {
            // Actually there should not be any error when can_recv is true.
            let n = tcp
                .recv_slice(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            if n > 0 {
                trace!("recv_slice {}", n);
                return Poll::Ready(Ok(n));
            } else {
                error!("Read zero when can_recv is true.");
            }
        }
        let state = tcp.state();
        if tcp.may_recv() ||
            // Reader could be used before the handshake is finished.
            state == TcpState::SynReceived
        {
            shared_state.lock().unwrap().waker = Some(cx.waker().clone());
            return task::Poll::Pending;
        } else {
            debug!("Socket may not receive in state {:?}", state);
        }
        if *eof_reached {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        *eof_reached = true;
        Poll::Ready(Ok(0))
    }
}

// // TODO AsyncRead for UdpReadHalf
// impl AsyncRead for UdpReadHalf {
//     fn poll_read(mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
//         let b = buf.initialize_unfilled();
//         self.as_mut()
//             .poll_read_inner(cx, b)
//             .map(|o| o.map(|n| buf.advance(n)))
//     }
// }
// // TODO futures::io::AsyncRead for UdpReadHalf
// impl futures::io::AsyncRead for UdpReadHalf {
//     fn poll_read(
//         mut self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//         buf: &mut [u8],
//     ) -> Poll<io::Result<usize>> {
//         self.as_mut().poll_read_inner(cx, buf)
//     }
// }

// TODO poll_read_inner for UdpReadHalf
impl UdpReadHalf{
    fn poll_read_inner(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let Self {
            ref mut mutex,
            ref mut shared_state,
            ref mut eof_reached,
            ..
        } = self;
        let l = mutex.poll_lock(cx);
        let mut guard = ready!(l);
        let udp = &mut guard.borrow_mut().udp;
        if udp.can_recv() {
            // Actually there should not be any error when can_recv is true.
            let (n,point) = udp
                .recv_slice(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            //TODO EndPoint
            if n > 0 {
                trace!("recv_slice {}", n);
                return Poll::Ready(Ok(n));
            } else {
                error!("Read zero when can_recv is true.");
            }
        }
        Poll::Ready(Ok(0))
    }
}

impl AsyncWrite for TcpWriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        self.as_mut().poll_write_inner(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        ready!(self.poll_close_inner(cx));
        Poll::Ready(Ok(()))
    }
}
impl futures::io::AsyncWrite for TcpWriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        self.as_mut().poll_write_inner(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        ready!(self.poll_close_inner(cx));
        Poll::Ready(Ok(()))
    }
}
impl TcpWriteHalf {
    fn poll_write_inner(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        let Self {
            ref mut mutex,
            ref mut shared_state,
            ref mut notifier,
            handle,
            ..
        } = self;
        let p = mutex.poll_lock(cx);
        let mut guard = ready!(p);
        let inactive = !guard.polling_active;
        let s = &mut guard.tcp;
        let n = if s.can_send() {
            let s = s
                .send_slice(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            trace!("Written {} bytes.", s);
            s
        } else {
            0
        };
        if inactive {
            notifier.send(handle.clone(), s.poll_at());
        }
        if n > 0 {
            return Poll::Ready(Ok(n));
        }
        if !s.may_send() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        trace!("Setting waker for writer.");
        shared_state.lock().unwrap().waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

// TODO AsyncWrite for UdpWriteHalf
impl AsyncWrite for UdpWriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        self.as_mut().poll_write_inner(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        ready!(self.poll_close_inner(cx));
        Poll::Ready(Ok(()))
    }
}
// TODO futures::io::AsyncWrite  for UdpWriteHalf
impl futures::io::AsyncWrite for UdpWriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        self.as_mut().poll_write_inner(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        ready!(self.poll_close_inner(cx));
        Poll::Ready(Ok(()))
    }
}
// TODO poll_write_inner for UdpWriteHalf
impl UdpWriteHalf{
    fn poll_write_inner(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        let Self {
            ref mut mutex,
            ref mut shared_state,
            ref mut notifier,
            handle,
            ..
        } = self;
        let p = mutex.poll_lock(cx);
        let mut guard = ready!(p);
        let inactive = !guard.polling_active;
        let s = &mut guard.udp;
        let n = if s.can_send() {
            let s = s
                .send_slice(buf,s.endpoint())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            1
        }else{
            0
        };
        if inactive {
            notifier.send(handle.clone(), s.poll_at());
        }
        if n > 0 {
            return Poll::Ready(Ok(n));
        }
        shared_state.lock().unwrap().waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
impl Drop for TcpReadHalf {
    fn drop(&mut self) {
        debug!("Drop ReadHalf {:?}", self.shutdown_notifier);
        self.shutdown_notifier.notify();
    }
}

impl Drop for TcpWriteHalf {
    fn drop(&mut self) {
        debug!("Drop WriteHalf {:?}", self.shutdown_notifier);
        self.shutdown_notifier.notify();
    }
}

// TODO Drop for UdpReadHalf
impl Drop for UdpReadHalf {
    fn drop(&mut self) {
        debug!("Drop ReadHalf {:?}", self.shutdown_notifier);
        self.shutdown_notifier.notify();
    }
}
// TODO Drop for UdpWriteHalf
impl Drop for UdpWriteHalf {
    fn drop(&mut self) {
        debug!("Drop WriteHalf {:?}", self.shutdown_notifier);
        self.shutdown_notifier.notify();
    }
}
impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "UsTcpStream({:?})", self.addr)?;
        Ok(())
    }
}
//TODO fmt::debug for UdpStream
impl fmt::Debug for UdpStream{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f,"UsUdpSteam({:?})",self.addr)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn t1() {}
}
