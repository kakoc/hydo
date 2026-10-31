//! Runtime pieces: a latch (Go's closed channel), waiting on several things at once, "the client
//! went away" (Go's request context), and the socket options Go sets on every TCP connection.

use std::future::{poll_fn, Future};
use std::os::fd::RawFd;
use std::pin::{pin, Pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::sync::Notify;

/// Fired once, waited on by many: what closing a channel is in Go.
#[derive(Debug, Default)]
pub struct Latch {
    fired: AtomicBool,
    n: Notify,
}

impl Latch {
    pub fn new() -> Arc<Latch> {
        Arc::new(Latch::default())
    }

    pub fn fire(&self) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            self.n.notify_waiters();
        }
    }

    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        loop {
            let mut f = pin!(self.n.notified());
            f.as_mut().enable();
            if self.fired() {
                return;
            }
            f.await;
        }
    }
}

/// The client of a request went away (Go: the request's context is done): the HTTP/1 client
/// closed its connection, the HTTP/2 stream was reset, or its connection died.
pub trait Gone: Send {
    fn poll_gone(&mut self, cx: &mut Context<'_>) -> Poll<()>;

    /// `ctx.Err() != nil`
    fn gone_now(&mut self) -> bool {
        let mut cx = Context::from_waker(Waker::noop());
        self.poll_gone(&mut cx).is_ready()
    }
}

/// A client that never goes away (the TCP proxy's holds end only by the rule or the heal).
pub struct Never;

impl Gone for Never {
    fn poll_gone(&mut self, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum End {
    Gone,
    Timer,
    Latch,
}

/// Wait until the client goes away, the time passes (if any) or a latch fires.
pub async fn wait(gone: &mut dyn Gone, d: Option<Duration>, latches: &[&Arc<Latch>]) -> End {
    let mut sleep = d.map(|d| Box::pin(tokio::time::sleep(d)));
    let mut ws: Vec<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = latches
        .iter()
        .map(|l| Box::pin(l.wait()) as Pin<Box<dyn Future<Output = ()> + Send>>)
        .collect();
    poll_fn(|cx| {
        if gone.poll_gone(cx).is_ready() {
            return Poll::Ready(End::Gone);
        }
        if let Some(s) = &mut sleep {
            if s.as_mut().poll(cx).is_ready() {
                return Poll::Ready(End::Timer);
            }
        }
        for w in &mut ws {
            if w.as_mut().poll(cx).is_ready() {
                return Poll::Ready(End::Latch);
            }
        }
        Poll::Pending
    })
    .await
}

/// Run f unless the client goes away first (`None` then).
pub async fn or_gone<F: Future>(gone: &mut dyn Gone, f: F) -> Option<F::Output> {
    let mut f = pin!(f);
    poll_fn(|cx| {
        if let Poll::Ready(x) = f.as_mut().poll(cx) {
            return Poll::Ready(Some(x));
        }
        if gone.poll_gone(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// The first of two futures.
pub enum Either<A, B> {
    L(A),
    R(B),
}

pub async fn first<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(x) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::L(x));
        }
        if let Poll::Ready(x) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::R(x));
        }
        Poll::Pending
    })
    .await
}

/// Run `main` to its end while `side` runs beside it (Go: a goroutine that the end of `main`
/// stops by closing what it reads); `side` is dropped when `main` ends.
pub async fn drive<A: Future, B: Future>(main: A, side: B) -> A::Output {
    let mut a = pin!(main);
    let mut b = pin!(side);
    let mut bdone = false;
    poll_fn(|cx| {
        if !bdone && b.as_mut().poll(cx).is_ready() {
            bdone = true;
        }
        a.as_mut().poll(cx)
    })
    .await
}

fn setopt(fd: RawFd, level: libc::c_int, name: libc::c_int, v: libc::c_int) {
    // SAFETY: fd is an open socket of the caller; v lives across the call.
    unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &v as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// What Go sets on every TCP connection it accepts or dials: no Nagle delay, and keep-alive
/// probes after 15 s idle, every 15 s, 9 of them.
pub fn tune(fd: RawFd) {
    setopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, 1);
    setopt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_os = "linux")]
    setopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 15);
    #[cfg(target_os = "macos")]
    setopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, 15);
    setopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 15);
    setopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 9);
}

/// SO_LINGER 0: closing the socket resets the connection.
pub fn zero_linger(fd: RawFd) {
    let l = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: fd is an open socket of the caller; l lives across the call.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &l as *const libc::linger as *const libc::c_void,
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latch_and_wait() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let l = Latch::new();
            let l2 = l.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                l2.fire();
            });
            assert_eq!(wait(&mut Never, None, &[&l]).await, End::Latch);
            assert_eq!(wait(&mut Never, None, &[&l]).await, End::Latch); // stays fired
            let m = Latch::new();
            assert_eq!(
                wait(&mut Never, Some(Duration::from_millis(5)), &[&m]).await,
                End::Timer
            );
        });
    }
}
