//! Management for flow control windows.
//!
//! Tor maintains a separate windows on circuits and on streams.
//! These are controlled by SENDME cells, which (confusingly) are
//! applied either at the circuit or the stream level depending on
//! whether they have a stream ID set.
//!
//! Circuit sendmes are _authenticated_: they include a cryptographic
//! tag generated by the cryptography layer.  This tag proves that the
//! other side of the circuit really has read all of the data that it's
//! acknowledging.

use futures::lock::Mutex;

use std::collections::VecDeque;
use std::sync::Arc;

use tor_cell::relaycell::msg::RelayMsg;
use tor_cell::relaycell::RelayCell;

use crate::{Error, Result};

// XXXX Three problems with this tag:
// XXXX - First, we need to support unauthenticated flow control.
// XXXX - Second, this tag type could be different for each layer, if we
// XXXX   eventually have an authenticator that isn't 20 bytes long.
// XXXX - Third, we want the comparison to happen with a constant-time
// XXXX   operation.

/// Tag type used in regular v1 sendme cells.
pub(crate) type CircTag = [u8; 20];
/// Absence of a tag, as with stream cells.
pub(crate) type NoTag = ();

/// A circuit's send window.
pub(crate) type CircSendWindow = SendWindow<CircParams, CircTag>;
/// A stream's send window.
pub(crate) type StreamSendWindow = SendWindow<StreamParams, NoTag>;

/// A circuit's receive window.
pub(crate) type CircRecvWindow = RecvWindow<CircParams>;
/// A stream's receive window.
pub(crate) type StreamRecvWindow = RecvWindow<StreamParams>;

/// Tracks how many cells we can safely send on a circuit or stream.
///
/// Additionally, remembers a list of tags that could be used to
/// acknowledge the cells we have already sent, so we know it's safe
/// to send more.
pub(crate) struct SendWindow<P, T>
where
    P: WindowParams,
    T: PartialEq + Eq + Clone,
{
    // TODO could use a bilock if that becomes non-experimental.
    // TODO I wish we could do this without locking; we could make a bunch
    // of these functions non-async if that happened.
    /// Actual SendWindow object.
    w: Arc<Mutex<SendWindowInner<T>>>,
    /// Marker type to tell the compiler that the P type is used.
    _dummy: std::marker::PhantomData<P>,
}

/// Interior (locked) code for SendWindowInner.
struct SendWindowInner<T>
where
    T: PartialEq + Eq + Clone,
{
    /// Current value for this window
    window: u16,
    /// Tag values that incoming "SENDME" messages need to match in order
    /// for us to send more data.
    tags: VecDeque<T>,
    /// An event to wait on if we find that we are out of cells.
    unblock: event_listener::Event,
}

/// Helper: parameterizes a window to determine its maximum and its increment.
pub(crate) trait WindowParams {
    /// Largest allowable value for this window.
    fn maximum() -> u16;
    /// Increment for this window.
    fn increment() -> u16;
}

/// Parameters used for SENDME windows on circuits: limit at 1000 cells,
/// and each SENDME adjusts by 100.
pub(crate) struct CircParams;
impl WindowParams for CircParams {
    fn maximum() -> u16 {
        1000
    }
    fn increment() -> u16 {
        100
    }
}

/// Parameters used for SENDME windows on streams: limit at 500 cells,
/// and each SENDME adjusts by 50.
#[derive(Clone)]
pub(crate) struct StreamParams;
impl WindowParams for StreamParams {
    fn maximum() -> u16 {
        500
    }
    fn increment() -> u16 {
        50
    }
}

impl<P, T> SendWindow<P, T>
where
    P: WindowParams,
    T: PartialEq + Eq + Clone,
{
    /// Construct a new SendWindow.
    pub(crate) fn new(window: u16) -> SendWindow<P, T> {
        let increment = P::increment();
        let capacity = (window + increment - 1) / increment;
        let inner = SendWindowInner {
            window,
            tags: VecDeque::with_capacity(capacity as usize),
            unblock: event_listener::Event::new(),
        };
        SendWindow {
            w: Arc::new(Mutex::new(inner)),
            _dummy: std::marker::PhantomData,
        }
    }

    /// Add a reference-count to SendWindow and return a new handle to it.
    pub(crate) fn new_ref(&self) -> Self {
        SendWindow {
            w: Arc::clone(&self.w),
            _dummy: std::marker::PhantomData,
        }
    }

    /// Remove one item from this window (since we've sent a cell).
    ///
    /// The provided tag is the one associated with the crypto layer that
    /// originated the cell.  It will get cloned and recorded if we'll
    /// need to check for it later.
    ///
    /// Return the number of cells left in the window.
    pub(crate) async fn take(&mut self, tag: &T) -> Result<u16> {
        loop {
            let wait_on = {
                let mut w = self.w.lock().await;
                if let Some(val) = w.window.checked_sub(1) {
                    w.window = val;
                    if w.window % P::increment() == 0 {
                        // We record this tag.
                        // TODO: I'm not saying that this cell in particular
                        // matches the spec, but Tor seems to like it.
                        w.tags.push_back(tag.clone());
                    }

                    return Ok(val);
                }

                // Window is zero; can't send yet.
                w.unblock.listen()
            };

            // Wait on this event while _not_ holding the lock.
            wait_on.await;
        }
    }

    /// Handle an incoming sendme with a provided tag.
    ///
    /// If the tag is None, then we don't enforce tag requirements. (We can
    /// remove this option once we no longer support getting SENDME cells
    /// from relays without the FlowCtrl=1 protocol.)
    ///
    /// On success, return the number of cells left in the window.
    ///
    /// On failure, return None: the caller should close the stream
    /// or circuit with a protocol error.
    #[must_use = "didn't check whether SENDME tag was right."]
    pub(crate) async fn put(&mut self, tag: Option<T>) -> Option<u16> {
        let mut w = self.w.lock().await;

        match (w.tags.front(), tag) {
            (Some(t), Some(tag)) if t == &tag => {} // this is the right tag.
            (Some(_), None) => {}                   // didn't need a tag.
            _ => {
                return None;
            } // Bad tag or unexpected sendme.
        }
        w.tags.pop_front();

        let was_zero = w.window == 0;

        let v = w.window.checked_add(P::increment())?;
        w.window = v;

        if was_zero {
            w.unblock.notify(usize::MAX)
        }
        Some(v)
    }

    /// For testing: get a copy of the current send window, and the
    /// expected incoming tags.
    #[cfg(test)]
    pub(crate) async fn window_and_expected_tags(&self) -> (u16, Vec<T>) {
        let inner = self.w.lock().await;
        let tags = inner.tags.iter().map(Clone::clone).collect();
        (inner.window, tags)
    }
}

/// Structure to track when we need to send SENDME cells for incoming data.
#[derive(Clone)]
pub(crate) struct RecvWindow<P: WindowParams> {
    /// Number of cells that we'd be willing to receive on this window
    /// before sending a SENDME.
    window: u16,
    /// Marker type to tell the compiler that the P type is used.
    _dummy: std::marker::PhantomData<P>,
}

impl<P: WindowParams> RecvWindow<P> {
    /// Create a new RecvWindow.
    pub(crate) fn new(window: u16) -> RecvWindow<P> {
        RecvWindow {
            window,
            _dummy: std::marker::PhantomData,
        }
    }

    /// Called when we've just sent a cell; return true if we need to send
    /// a sendme, and false otherwise.
    ///
    /// Returns None if we should not have sent the cell, and we just
    /// violated the window.
    pub(crate) fn take(&mut self) -> Result<bool> {
        let v = self.window.checked_sub(1);
        if let Some(x) = v {
            self.window = x;
            // TODO: same note as in SendWindow.take(). I don't know if
            // this truly matches the spec, but tor accepts it.
            Ok(x % P::increment() == 0)
        } else {
            Err(Error::CircProto(
                "Received a data cell in violation of a window".into(),
            ))
        }
    }

    /// Reduce this window by `n`; give an error if this is not possible.
    pub(crate) fn decrement_n(&mut self, n: u16) -> crate::Result<()> {
        let v = self.window.checked_sub(n);
        if let Some(x) = v {
            self.window = x;
            Ok(())
        } else {
            Err(crate::Error::CircProto(
                "Received too many cells on a stream".into(),
            ))
        }
    }

    /// Called when we've just sent a SENDME.
    pub(crate) fn put(&mut self) {
        self.window = self
            .window
            .checked_add(P::increment())
            .expect("Overflow detected while attempting to increment window");
    }
}

/// Return true if this message is counted by flow-control windows.
pub(crate) fn msg_counts_towards_windows(msg: &RelayMsg) -> bool {
    matches!(msg, RelayMsg::Data(_))
}

/// Return true if this message is counted by flow-control windows.
pub(crate) fn cell_counts_towards_windows(cell: &RelayCell) -> bool {
    msg_counts_towards_windows(cell.msg())
}

#[cfg(test)]
mod test {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use futures::FutureExt;
    use tokio::test as async_test;
    use tokio_crate as tokio;
    use tor_cell::relaycell::{msg, RelayCell};

    #[test]
    fn what_counts() {
        let m = msg::Begin::new("www.torproject.org", 443, 0)
            .unwrap()
            .into();
        assert!(!msg_counts_towards_windows(&m));
        assert!(!cell_counts_towards_windows(&RelayCell::new(77.into(), m)));

        let m = msg::Data::new(&b"Education is not a prerequisite to political control-political control is the cause of popular education."[..]).unwrap().into(); // Du Bois
        assert!(msg_counts_towards_windows(&m));
        assert!(cell_counts_towards_windows(&RelayCell::new(128.into(), m)));
    }

    #[test]
    fn recvwindow() {
        let mut w: RecvWindow<StreamParams> = RecvWindow::new(500);

        for _ in 0..49 {
            assert!(!w.take().unwrap());
        }
        assert!(w.take().unwrap());
        assert_eq!(w.window, 450);

        assert!(w.decrement_n(123).is_ok());
        assert_eq!(w.window, 327);

        w.put();
        assert_eq!(w.window, 377);

        // failing decrement.
        assert!(w.decrement_n(400).is_err());
        // failing take.
        assert!(w.decrement_n(377).is_ok());
        assert!(w.take().is_err());
    }

    fn new_sendwindow() -> SendWindow<CircParams, &'static str> {
        SendWindow::new(1000)
    }

    #[async_test]
    async fn sendwindow_basic() -> Result<()> {
        let mut w = new_sendwindow();

        let n = w.take(&"Hello").await?;
        assert_eq!(n, 999);
        for _ in 0_usize..98 {
            w.take(&"world").await?;
        }
        assert_eq!(w.w.lock().await.window, 901);
        assert_eq!(w.w.lock().await.tags.len(), 0);

        let n = w.take(&"and").await?;
        assert_eq!(n, 900);
        assert_eq!(w.w.lock().await.tags.len(), 1);
        assert_eq!(w.w.lock().await.tags[0], "and");

        let n = w.take(&"goodbye").await?;
        assert_eq!(n, 899);
        assert_eq!(w.w.lock().await.tags.len(), 1);

        // Try putting a good tag.
        let n = w.put(Some("and")).await;
        assert_eq!(n, Some(999));
        assert_eq!(w.w.lock().await.tags.len(), 0);

        for _ in 0_usize..300 {
            w.take(&"dreamland").await?;
        }
        assert_eq!(w.w.lock().await.tags.len(), 3);

        // Put without a tag.
        let n = w.put(None).await;
        assert_eq!(n, Some(799));
        assert_eq!(w.w.lock().await.tags.len(), 2);

        Ok(())
    }

    #[async_test]
    async fn sendwindow_bad_put() -> Result<()> {
        let mut w = new_sendwindow();
        for _ in 0_usize..250 {
            w.take(&"correct").await?;
        }

        // wrong tag: won't work.
        assert_eq!(w.w.lock().await.window, 750);
        let n = w.put(Some("incorrect")).await;
        assert!(n.is_none());

        let n = w.put(Some("correct")).await;
        assert_eq!(n, Some(850));
        let n = w.put(Some("correct")).await;
        assert_eq!(n, Some(950));

        // no tag expected: won't work.
        let n = w.put(Some("correct")).await;
        assert_eq!(n, None);
        assert_eq!(w.w.lock().await.window, 950);

        let n = w.put(None).await;
        assert_eq!(n, None);
        assert_eq!(w.w.lock().await.window, 950);

        Ok(())
    }

    #[async_test]
    async fn sendwindow_blocking() -> Result<()> {
        let mut w = new_sendwindow();
        for _ in 0_usize..1000 {
            w.take(&"here a string").await?;
        }
        assert_eq!(w.w.lock().await.window, 0);

        // This is going to block -- make sure it doesn't say it's ready.
        let ready = w.take(&"there a string").now_or_never();
        assert!(ready.is_none());

        // TODO: test that this actually wakes up when somebody else says "put".
        Ok(())
    }
}
