//! A mock type implementing [`AsyncRead`] and [`AsyncWrite`].
//!
//!
//! # Overview
//!
//! Provides a type that implements [`AsyncRead`] + [`AsyncWrite`] that can be configured
//! to handle an arbitrary sequence of read and write operations. This is useful
//! for writing unit tests for networking services as using an actual network
//! type is fairly non deterministic.
//!
//! # Usage
//!
//! Attempting to write data that the mock isn't expecting will result in a
//! panic.
//!
//! [`AsyncRead`]: tokio::io::AsyncRead
//! [`AsyncWrite`]: tokio::io::AsyncWrite

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::{self, Duration, Instant, Sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;

use futures_core::Stream;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{self, ready, Poll, Waker};
use std::{cmp, io};

/// An I/O object that follows a predefined script.
///
/// This value is created by `Builder` and implements `AsyncRead` + `AsyncWrite`. It
/// follows the scenario described by the builder and panics otherwise.
#[derive(Debug)]
pub struct Mock {
    inner: Inner,
}

/// A handle to send additional actions to the related `Mock`.
#[derive(Debug)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Action>,
}

/// Builds `Mock` instances.
#[derive(Debug, Clone, Default)]
pub struct Builder {
    // Sequence of actions for the Mock to take
    actions: VecDeque<Action>,
    name: String,
}

#[derive(Debug, Clone)]
enum Action {
    Read(Vec<u8>),
    Write(Vec<u8>),
    Wait(Duration),
    // Wrapped in Arc so that Builder can be cloned and Send.
    // Mock is not cloned as does not need to check Rc for ref counts.
    ReadError(Option<Arc<io::Error>>),
    WriteError(Option<Arc<io::Error>>),
    ReadZeroes(usize),
    WriteZeroes(usize),
    IgnoreWritten(usize),
    ReadEof(bool),
    StopChecking,
}

struct Inner {
    actions: VecDeque<Action>,
    waiting: Option<Instant>,
    sleep: Option<Pin<Box<Sleep>>>,
    read_wait: Option<Waker>,
    rx: UnboundedReceiverStream<Action>,
    name: String,
    checks_enabled: bool,
}

impl Builder {
    /// Return a new, empty `Builder`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Read(buf.into()));
        self
    }

    /// Sequence a `read` operation, resuting in a specified number of zero bytes.
    ///
    /// Same as [`Self::read`] with aa zero buffer, but with less memory consumption.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn read_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::ReadZeroes(nbytes));
        self
    }

    /// Sequence a read operation that will return 0, i.e. end of file.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn eof(&mut self) -> &mut Self {
        self.actions.push_back(Action::ReadEof(false));
        self
    }

    /// Sequence a `read` operation that produces an error.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `error`.
    pub fn read_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.actions.push_back(Action::ReadError(error));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Write(buf.into()));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will be asserted to be equal to zero.
    pub fn write_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::WriteZeroes(nbytes));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will not be checked.
    pub fn write_ignore(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::IgnoreWritten(nbytes));
        self
    }

    /// Sequence a `write` operation that produces an error.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call that provides `error`.
    pub fn write_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.actions.push_back(Action::WriteError(error));
        self
    }

    /// Sequence a special event that makes this Mock stop asserting any operation (i.e. allow everything).
    ///
    /// Reaching this means the test is already succeeded and possible
    /// further operations are likely irrelevent.
    ///
    /// More reads can still be sequenced after this.
    pub fn stop_checking(&mut self) -> &mut Self {
        self.actions.push_back(Action::StopChecking);
        self
    }

    /// Sequence a wait.
    ///
    /// The next operation in the mock's script will be to wait without doing so
    /// for `duration` amount of time.
    pub fn wait(&mut self, duration: Duration) -> &mut Self {
        let duration = cmp::max(duration, Duration::from_millis(1));
        self.actions.push_back(Action::Wait(duration));
        self
    }

    /// Set name of the mock IO object to include in panic messages and debug output
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into();
        self
    }

    /// Build a `Mock` value according to the defined script.
    pub fn build(&mut self) -> Mock {
        let (mock, _) = self.build_with_handle();
        mock
    }

    /// Build a `Mock` value paired with a handle
    pub fn build_with_handle(&mut self) -> (Mock, Handle) {
        let (inner, handle) = Inner::new(self.actions.clone(), self.name.clone());

        let mock = Mock { inner };

        (mock, handle)
    }
}

impl Handle {
    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.tx.send(Action::Read(buf.into())).unwrap();
        self
    }

    /// Sequence a `read` operation, resuting in a specified number of zero bytes.
    ///
    /// Same as [`Self::read`] with aa zero buffer, but with less memory consumption.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn read_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::ReadZeroes(nbytes)).unwrap();
        self
    }

    /// Sequence a read operation that will return 0, i.e. end of file.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn eof(&mut self) -> &mut Self {
        self.tx.send(Action::ReadEof(false)).unwrap();
        self
    }

    /// Sequence a `read` operation error.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `error`.
    pub fn read_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.tx.send(Action::ReadError(error)).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.tx.send(Action::Write(buf.into())).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will be asserted to be equal to zero.
    pub fn write_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::WriteZeroes(nbytes)).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will not be checked.
    pub fn write_ignore(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::IgnoreWritten(nbytes)).unwrap();
        self
    }

    /// Sequence a `write` operation error.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call error.
    pub fn write_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.tx.send(Action::WriteError(error)).unwrap();
        self
    }

    /// Sequence a special event that makes this Mock stop asserting any operation (i.e. allow everything).
    ///
    /// Reaching this means the test is already succeeded and possible
    /// further operations are likely irrelevent.
    ///
    /// More reads can still be sequenced after this.
    pub fn stop_checking(&mut self) -> &mut Self {
        self.tx.send(Action::StopChecking).unwrap();
        self
    }
}

impl Inner {
    fn new(actions: VecDeque<Action>, name: String) -> (Inner, Handle) {
        let (tx, rx) = mpsc::unbounded_channel();

        let rx = UnboundedReceiverStream::new(rx);

        let inner = Inner {
            actions,
            sleep: None,
            read_wait: None,
            rx,
            waiting: None,
            name,
            checks_enabled: true,
        };

        let handle = Handle { tx };

        (inner, handle)
    }

    fn poll_action(&mut self, cx: &mut task::Context<'_>) -> Poll<Option<Action>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }

    fn read(&mut self, dst: &mut ReadBuf<'_>) -> io::Result<()> {
        match self.action() {
            Some(&mut Action::ReadZeroes(ref mut nbytes)) => {
                let n = cmp::min(dst.remaining(), *nbytes);
                let nfilled = dst.filled().len();
                dst.initialize_unfilled_to(nfilled + n)[nfilled..].fill(0);
                dst.set_filled(nfilled + n);
                *nbytes -= n;
                Ok(())
            }
            Some(&mut Action::ReadEof(ref mut observed)) => {
                *observed = true;
                Ok(())
            }
            Some(&mut Action::Read(ref mut data)) => {
                // Figure out how much to copy
                let n = cmp::min(dst.remaining(), data.len());

                // Copy the data into the `dst` slice
                dst.put_slice(&data[..n]);

                // Drain the data from the source
                data.drain(..n);

                Ok(())
            }
            Some(&mut Action::ReadError(ref mut err)) => {
                // As the
                let err = err.take().expect("Should have been removed from actions.");
                let err = Arc::try_unwrap(err).expect("There are no other references.");
                Err(err)
            }
            Some(_) => {
                // Either waiting or expecting a write
                Err(io::ErrorKind::WouldBlock.into())
            }
            None => Ok(()),
        }
    }

    fn write(&mut self, mut src: &[u8]) -> io::Result<usize> {
        let mut ret = 0;

        if self.actions.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }

        if let Some(&mut Action::Wait(..)) = self.action() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        if let Some(&mut Action::WriteError(ref mut err)) = self.action() {
            let err = err.take().expect("Should have been removed from actions.");
            let err = Arc::try_unwrap(err).expect("There are no other references.");
            return Err(err);
        }

        let mut checks_enabled = self.checks_enabled;

        for i in 0..self.actions.len() {
            let action = &mut self.actions[i];
            let ignore_written = matches!(action, Action::IgnoreWritten { .. });
            match action {
                Action::Write(ref mut expect) => {
                    let n = cmp::min(src.len(), expect.len());

                    if self.checks_enabled {
                        assert_eq!(&src[..n], &expect[..n], "name={} i={}", self.name, i);
                    }

                    // Drop data that was matched
                    expect.drain(..n);
                    src = &src[n..];

                    ret += n;

                    if src.is_empty() {
                        return Ok(ret);
                    }
                }
                Action::WriteZeroes(ref mut nbytes) | Action::IgnoreWritten(ref mut nbytes) => {
                    let n = cmp::min(src.len(), *nbytes);

                    if checks_enabled && !ignore_written {
                        for (j, x) in src[..n].iter().enumerate() {
                            assert_eq!(
                                *x,
                                0,
                                "byte_index={j} name={} remaining actions: {}",
                                self.name,
                                self.actions.len() - i
                            );
                        }
                    }

                    // Drop data that was matched
                    *nbytes -= n;
                    src = &src[n..];

                    ret += n;

                    if src.is_empty() {
                        return Ok(ret);
                    }
                }
                Action::StopChecking => checks_enabled = false,
                Action::Wait(..) | Action::WriteError(..) => {
                    break;
                }
                _ => {}
            }

            // TODO: remove write
        }

        Ok(ret)
    }

    fn remaining_wait(&mut self) -> Option<Duration> {
        match self.action() {
            Some(&mut Action::Wait(dur)) => Some(dur),
            _ => None,
        }
    }

    fn action(&mut self) -> Option<&mut Action> {
        loop {
            if self.actions.is_empty() {
                return None;
            }

            match self.actions[0] {
                Action::Read(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::ReadZeroes(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::ReadEof(ref observed) => {
                    if !observed {
                        break;
                    }
                }
                Action::Write(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::WriteZeroes(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::IgnoreWritten(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::Wait(ref mut dur) => {
                    if let Some(until) = self.waiting {
                        let now = Instant::now();

                        if now < until {
                            break;
                        } else {
                            self.waiting = None;
                        }
                    } else {
                        self.waiting = Some(Instant::now() + *dur);
                        break;
                    }
                }
                Action::ReadError(ref mut error) | Action::WriteError(ref mut error) => {
                    if error.is_some() {
                        break;
                    }
                }
                Action::StopChecking => {
                    self.checks_enabled = false;
                    break;
                }
            }

            let _action = self.actions.pop_front();
        }

        self.actions.front_mut()
    }
}

// ===== impl Inner =====

impl Mock {
    fn maybe_wakeup_reader(&mut self) {
        match self.inner.action() {
            Some(&mut Action::Read(_)) | Some(&mut Action::ReadError(_)) | None => {
                if let Some(waker) = self.inner.read_wait.take() {
                    waker.wake();
                }
            }
            _ => {}
        }
    }
}

impl AsyncRead for Mock {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if let Some(ref mut sleep) = self.inner.sleep {
                ready!(Pin::new(sleep).poll(cx));
            }

            // If a sleep is set, it has already fired
            self.inner.sleep = None;

            // Capture 'filled' to monitor if it changed
            let filled = buf.filled().len();

            match self.inner.read(buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(rem) = self.inner.remaining_wait() {
                        let until = Instant::now() + rem;
                        self.inner.sleep = Some(Box::pin(time::sleep_until(until)));
                    } else {
                        self.inner.read_wait = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                }
                Ok(()) => {
                    if buf.filled().len() == filled {
                        match ready!(self.inner.poll_action(cx)) {
                            Some(action) => {
                                self.inner.actions.push_back(action);
                                continue;
                            }
                            None => {
                                return Poll::Ready(Ok(()));
                            }
                        }
                    } else {
                        return Poll::Ready(Ok(()));
                    }
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
}

impl AsyncWrite for Mock {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            if let Some(ref mut sleep) = self.inner.sleep {
                ready!(Pin::new(sleep).poll(cx));
            }

            // If a sleep is set, it has already fired
            self.inner.sleep = None;

            if self.inner.actions.is_empty() {
                match self.inner.poll_action(cx) {
                    Poll::Pending => {
                        // do not propagate pending
                    }
                    Poll::Ready(Some(action)) => {
                        self.inner.actions.push_back(action);
                    }
                    Poll::Ready(None) => {
                        if self.inner.checks_enabled {
                            panic!("unexpected write {}", self.pmsg());
                        } else {
                            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                        }
                    }
                }
            }

            match self.inner.write(buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(rem) = self.inner.remaining_wait() {
                        let until = Instant::now() + rem;
                        self.inner.sleep = Some(Box::pin(time::sleep_until(until)));
                    } else {
                        if self.inner.checks_enabled {
                            panic!("unexpected WouldBlock {}", self.pmsg());
                        } else {
                            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                        }
                    }
                }
                Ok(0) => {
                    // TODO: Is this correct?
                    if !self.inner.actions.is_empty() {
                        return Poll::Pending;
                    }

                    // TODO: Extract
                    match ready!(self.inner.poll_action(cx)) {
                        Some(action) => {
                            self.inner.actions.push_back(action);
                            continue;
                        }
                        None => {
                            if self.inner.checks_enabled {
                                panic!("unexpected write {}", self.pmsg());
                            } else {
                                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                            }
                        }
                    }
                }
                ret => {
                    self.maybe_wakeup_reader();
                    return Poll::Ready(ret);
                }
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Ensures that Mock isn't dropped with data "inside".
impl Drop for Mock {
    fn drop(&mut self) {
        // Avoid double panicking, since makes debugging much harder.
        if std::thread::panicking() {
            return;
        }

        if !self.inner.checks_enabled {
            return;
        }

        self.inner.actions.iter().for_each(|a| match a {
            Action::Read(data) => assert!(
                data.is_empty(),
                "There is still data left to read. {}",
                self.pmsg()
            ),
            Action::ReadZeroes(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to read. {}",
                self.pmsg()
            ),
            Action::ReadEof(observed) => assert!(
                observed,
                "There is still a read EOF event that was not observed {}",
                self.pmsg()
            ),
            Action::Write(data) => assert!(
                data.is_empty(),
                "There is still data left to write. {}",
                self.pmsg()
            ),
            Action::WriteZeroes(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to write. {}",
                self.pmsg()
            ),
            Action::IgnoreWritten(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to write (even though content is to be ignored). {}",
                self.pmsg()
            ),
            _ => (),
        });
    }
}
/*
/// Returns `true` if called from the context of a futures-rs Task
fn is_task_ctx() -> bool {
    use std::panic;

    // Save the existing panic hook
    let h = panic::take_hook();

    // Install a new one that does nothing
    panic::set_hook(Box::new(|_| {}));

    // Attempt to call the fn
    let r = panic::catch_unwind(|| task::current()).is_ok();

    // Re-install the old one
    panic::set_hook(h);

    // Return the result
    r
}
*/

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "Inner {{...}}")
        } else {
            write!(f, "Inner {{name={}, ...}}", self.name)
        }
    }
}

struct PanicMsgSnippet<'a>(&'a Inner);

impl<'a> fmt::Display for PanicMsgSnippet<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.name.is_empty() {
            write!(f, "({} actions remain)", self.0.actions.len())
        } else {
            write!(
                f,
                "(name {}, {} actions remain)",
                self.0.name,
                self.0.actions.len()
            )
        }
    }
}

impl Mock {
    fn pmsg(&self) -> PanicMsgSnippet<'_> {
        PanicMsgSnippet(&self.inner)
    }
}
