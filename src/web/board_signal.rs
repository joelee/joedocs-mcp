//! The Board's change signal and its stream (JD-47 D-05, D-06).
//!
//! One watcher task per server reads the store's change marker every
//! [`TICK`] while at least one Board stream is open, holding the store lock
//! only for the read, so it sees every writer, a local `joedocs` command
//! included. It coalesces changes: the first after a quiet window is
//! published at once, later ones at most once per `admin.board_refresh_ms`.
//! Each `GET /admin/board/stream` turns the published generations into
//! Server-Sent Events that say only that something changed, and when; the
//! page fetches the grid again. The stream sends a comment line every
//! keep-alive interval, checks the session then, and ends when the session
//! has lapsed, when the server shuts down, or when the browser goes away.
//!
//! `GET /admin/documents/{id}/stream` (JD-65 D-07) rides the same watcher
//! and the same places: on each published change it reads the document's
//! revision from the index, and sends an event only when that document
//! moved, so the page fetches itself again only for its own changes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{Route, WebError, WebRequest, WebState, open_session, session, write_audit};

/// Most streams open at once on one server (D-06): Board and document
/// streams share them.
pub(crate) const MAX_BOARD_STREAMS: usize = 32;

/// How often the watcher reads the change marker.
const TICK: Duration = Duration::from_millis(200);

/// What the watcher publishes: how many changes it has announced, and when
/// (Unix milliseconds) the latest one was first seen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Signal {
    pub(crate) generation: u64,
    pub(crate) seen_ms: u128,
}

/// The coalescing rules, apart from time and tasks so they can be tested
/// with chosen instants.
#[derive(Debug)]
pub(crate) struct Coalescer {
    last: Option<String>,
    pending: Option<u128>,
    sent_at: Option<Instant>,
    window: Duration,
    generation: u64,
}

impl Coalescer {
    /// Starts from `marker`, the state the first viewer's page showed.
    pub(crate) fn new(marker: Option<String>, window: Duration) -> Self {
        Self {
            last: marker,
            pending: None,
            sent_at: None,
            window,
            generation: 0,
        }
    }

    /// Takes the marker read at `now` (`wall_ms` the same moment in Unix
    /// milliseconds); returns a signal to publish, if one is due.
    pub(crate) fn observe(
        &mut self,
        marker: Option<String>,
        now: Instant,
        wall_ms: u128,
    ) -> Option<Signal> {
        if marker != self.last {
            self.last = marker;
            self.pending.get_or_insert(wall_ms);
        }
        let quiet = self
            .sent_at
            .is_none_or(|at| now.duration_since(at) >= self.window);
        let seen = self.pending.filter(|_| quiet)?;
        self.pending = None;
        self.sent_at = Some(now);
        self.generation += 1;
        Some(Signal {
            generation: self.generation,
            seen_ms: seen,
        })
    }
}

/// Reads the store's change marker (a blocking call).
pub(crate) type MarkerSource = dyn Fn() -> Option<String> + Send + Sync;

/// The watcher's shared state.
#[derive(Debug, Default)]
struct Watching {
    running: bool,
    /// The marker the watcher last read.
    known: Option<String>,
}

/// The server's change signal: the watcher, its subscribers and the count
/// of open streams.
pub(crate) struct BoardSignal {
    read: Arc<MarkerSource>,
    window: Duration,
    shutdown: CancellationToken,
    tx: watch::Sender<Signal>,
    state: Mutex<Watching>,
    open: AtomicUsize,
}

/// A place among the open streams, given back when dropped.
pub(crate) struct OpenStream(Arc<BoardSignal>);

impl Drop for OpenStream {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BoardSignal {
    pub(crate) fn new(
        read: Arc<MarkerSource>,
        window: Duration,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            read,
            window,
            shutdown,
            tx: watch::channel(Signal::default()).0,
            state: Mutex::new(Watching::default()),
            open: AtomicUsize::new(0),
        })
    }

    /// A place among the open streams, or `None` when all are taken.
    pub(crate) fn try_open(self: &Arc<Self>) -> Option<OpenStream> {
        self.open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_BOARD_STREAMS).then_some(n + 1)
            })
            .ok()
            .map(|_| OpenStream(Arc::clone(self)))
    }

    /// Subscribes a stream whose page showed `marker`, starting the
    /// watcher if none runs. Returns the receiver and whether the page is
    /// already behind what the watcher last read.
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        marker: Option<String>,
    ) -> (watch::Receiver<Signal>, bool) {
        let rx = self.tx.subscribe();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.running {
            return (rx, state.known != marker);
        }
        state.running = true;
        state.known.clone_from(&marker);
        tokio::spawn(Arc::clone(self).watch(marker));
        (rx, false)
    }

    /// Whether the watcher task runs (it stops when no stream is open).
    #[cfg(test)]
    fn running(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
    }

    async fn watch(self: Arc<Self>, marker: Option<String>) {
        let mut coalescer = Coalescer::new(marker, self.window);
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                () = self.shutdown.cancelled() => break,
            }
            if self.tx.receiver_count() == 0 {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                // Checked again under the lock: a stream may have just come.
                if self.tx.receiver_count() == 0 {
                    state.running = false;
                    return;
                }
            }
            let read = Arc::clone(&self.read);
            let Ok(marker) = tokio::task::spawn_blocking(move || read()).await else {
                break;
            };
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .known
                .clone_from(&marker);
            if let Some(signal) = coalescer.observe(marker, Instant::now(), wall_ms()) {
                self.tx.send_replace(signal);
            }
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running = false;
    }
}

fn wall_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn event(signal: Signal) -> Bytes {
    Bytes::from(format!(
        "id: {0}\ndata: {0} {1}\n\n",
        signal.generation, signal.seen_ms
    ))
}

/// What a stream follows.
pub(super) enum Follow {
    /// Every change: the Board fetches its grid again.
    Board,
    /// One live document's revisions, for its page.
    Document(String),
}

/// `GET /admin/board/stream?marker=<the grid's data-marker>`, or
/// `GET /admin/documents/{id}/stream?revision=<the page's revision>`.
pub(super) async fn stream(
    state: Arc<WebState>,
    request: WebRequest,
    route: Route,
    follow: Follow,
) -> Response {
    let marker = request
        .query("marker")
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    let shown = request
        .query("revision")
        .and_then(|r| r.parse::<u64>().ok())
        .unwrap_or(0);
    let opening = Arc::clone(&state);
    let checking = request.clone();
    let document = match &follow {
        Follow::Document(id) => Some(id.clone()),
        Follow::Board => None,
    };
    let operation = route.operation();
    // A document stream listens before it reads the document's revision
    // (JD-66 MED-04): a change published between the two is then in the
    // receiver, and one before the read is in the revision. A refused
    // request drops its receiver with the rest.
    let following_document = matches!(follow, Follow::Document(_));
    let signal = Arc::clone(&state.board_signal);
    let subscribe = move || following_document.then(|| signal.subscribe(None).0);
    let (early, opened) = subscribe_then_read(subscribe, || {
        tokio::task::spawn_blocking(move || {
            let Ok(store) = opening.shared.store().lock() else {
                return Err(WebError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                ));
            };
            let (who, result) = if session::same_origin(&checking.headers, &opening.allowed_origins)
            {
                match open_session(&store, &opening, &checking) {
                    Ok(session) => {
                        // A document stream needs a live document: its revision
                        // now, for a page already behind it.
                        let current = document
                            .as_deref()
                            .map(|id| store.document_info(id).map(|info| info.revision))
                            .transpose()
                            .map_err(|e| WebError::from_store(&e));
                        let place = current.and_then(|current| {
                            let place = opening.board_signal.try_open().ok_or_else(|| {
                                WebError::new(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "busy",
                                    "Too many open live pages; close one and reload.",
                                )
                            })?;
                            Ok((place, current))
                        });
                        (Some(session.admin), place)
                    }
                    Err(refused) => (None, Err(refused)),
                }
            } else {
                (
                    None,
                    Err(WebError::forbidden(
                        "this stream is for this site's pages only",
                    )),
                )
            };
            let outcome = result.as_ref().map_or_else(|e| e.code, |_| "ok");
            write_audit(&store, who.as_ref(), operation, outcome);
            result
        })
    })
    .await;
    let (place, current) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(refused)) => return refused.into_response(),
        Err(error) => {
            tracing::error!(%error, "stream task failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let (tx, body): (Outgoing, _) = mpsc::channel(8);
    match follow {
        Follow::Board => {
            let (rx, behind) = state.board_signal.subscribe(marker);
            tokio::spawn(feed(state, request, place, rx, behind, tx));
        }
        Follow::Document(id) => {
            let rx = early.unwrap_or_else(|| state.board_signal.subscribe(None).0);
            let current = current.unwrap_or(shown);
            tokio::spawn(document_feed(
                state, request, place, rx, id, shown, current, tx,
            ));
        }
    }
    let mut response =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// Writes one stream's events until it should end, holding its place
/// among the open streams.
async fn feed(
    state: Arc<WebState>,
    request: WebRequest,
    place: OpenStream,
    rx: watch::Receiver<Signal>,
    behind: bool,
    tx: Outgoing,
) {
    let _place = place;
    let shutdown = state.board_signal.shutdown.clone();
    let keep_alive = state.options.board.keep_alive;
    let session = || session_holds(&state, &request);
    run_feed(rx, behind, tx, &shutdown, keep_alive, session).await;
}

/// Runs `subscribe`, then starts and awaits `read`, in that order:
/// whatever is published after the subscription is seen by its receiver,
/// and whatever happened before the read is in its result, so nothing
/// falls between. `read` is a function, so nothing it spawns can start
/// before the subscription.
async fn subscribe_then_read<S, F>(
    subscribe: impl FnOnce() -> S,
    read: impl FnOnce() -> F,
) -> (S, F::Output)
where
    F: std::future::Future,
{
    let subscribed = subscribe();
    (subscribed, read().await)
}

/// The stream's outgoing body.
type Outgoing = mpsc::Sender<Result<Bytes, std::convert::Infallible>>;

/// What a published change becomes on one stream.
enum Turn {
    /// An event to send.
    Send(Bytes),
    /// Nothing for this stream.
    Skip,
    /// A last event, then the stream ends.
    End(Bytes),
}

fn revision_event(revision: u64) -> Bytes {
    Bytes::from(format!("event: revision\ndata: {revision}\n\n"))
}

/// A document stream's events until it should end: its revision at once
/// when the page (`shown`) is behind `current`, then one per published
/// change that moved the document, and `event: gone` when it is archived
/// or removed.
#[expect(
    clippy::too_many_arguments,
    reason = "the feed's parts, as `feed` takes them, plus the document"
)]
async fn document_feed(
    state: Arc<WebState>,
    request: WebRequest,
    place: OpenStream,
    rx: watch::Receiver<Signal>,
    id: String,
    shown: u64,
    current: u64,
    tx: Outgoing,
) {
    let _place = place;
    let shutdown = state.board_signal.shutdown.clone();
    let keep_alive = state.options.board.keep_alive;
    let session = || session_holds(&state, &request);
    let catch_up = (current > shown).then(|| revision_event(current));
    let last = Arc::new(std::sync::atomic::AtomicU64::new(current.max(shown)));
    let turn = |_: Signal| {
        let (state, id, last) = (Arc::clone(&state), id.clone(), Arc::clone(&last));
        async move {
            let read = tokio::task::spawn_blocking(move || {
                state
                    .shared
                    .store()
                    .lock()
                    .map(|store| store.document_info(&id).map(|info| info.revision))
                    .ok()
            })
            .await
            .ok()
            .flatten();
            match read {
                Some(Ok(revision)) if revision > last.load(Ordering::Acquire) => {
                    last.store(revision, Ordering::Release);
                    Turn::Send(revision_event(revision))
                }
                Some(Err(crate::store::Error::NotFound(_) | crate::store::Error::Archived(_))) => {
                    Turn::End(Bytes::from_static(b"event: gone\ndata: -\n\n"))
                }
                _ => Turn::Skip,
            }
        }
    };
    run_feed_with(rx, catch_up, tx, &shutdown, keep_alive, session, turn).await;
}

/// One stream's event loop, apart from the server so tests can drive it:
/// the retry line, a catch-up event when the page is `behind`, then an
/// event per published signal and a comment line every `keep_alive`, when
/// `session` (checked then) still holds.
async fn run_feed<S, F>(
    rx: watch::Receiver<Signal>,
    behind: bool,
    tx: Outgoing,
    shutdown: &CancellationToken,
    keep_alive: Duration,
    session: S,
) where
    S: Fn() -> F,
    F: std::future::Future<Output = bool>,
{
    let catch_up = behind.then(|| {
        event(Signal {
            generation: rx.borrow().generation,
            seen_ms: wall_ms(),
        })
    });
    let turn = |signal: Signal| std::future::ready(Turn::Send(event(signal)));
    run_feed_with(rx, catch_up, tx, shutdown, keep_alive, session, turn).await;
}

/// The event loop of any stream: the retry line, `catch_up` when there is
/// one, then what `turn` makes of each published signal, and a comment
/// line every `keep_alive`, when `session` (checked then) still holds.
async fn run_feed_with<S, F, T, G>(
    mut rx: watch::Receiver<Signal>,
    catch_up: Option<Bytes>,
    tx: Outgoing,
    shutdown: &CancellationToken,
    keep_alive: Duration,
    session: S,
    mut turn: T,
) where
    S: Fn() -> F,
    F: std::future::Future<Output = bool>,
    T: FnMut(Signal) -> G,
    G: std::future::Future<Output = Turn>,
{
    // Every send gives way to shutdown and to the client going away: a
    // stream never waits to deliver one more event before it ends.
    let send = |bytes: Bytes| deliver(&tx, shutdown, bytes);
    if !send(Bytes::from_static(b"retry: 2000\n\n")).await {
        return;
    }
    if let Some(bytes) = catch_up
        && !send(bytes).await
    {
        return;
    }
    // No `mark_unchanged` here: a receiver starts with the current value
    // seen, so whatever the watcher publishes after the stream subscribed,
    // even before this loop runs, is still sent (an extra refresh at
    // worst, never a missed one).
    let mut beat = tokio::time::interval(keep_alive);
    beat.reset();
    loop {
        let bytes = tokio::select! {
            changed = rx.changed() => match changed {
                Ok(()) => {
                    let signal = *rx.borrow_and_update();
                    match turn(signal).await {
                        Turn::Send(bytes) => bytes,
                        Turn::Skip => continue,
                        Turn::End(bytes) => {
                            send(bytes).await;
                            break;
                        }
                    }
                }
                Err(_) => break,
            },
            _ = beat.tick() => {
                if !session().await {
                    send(Bytes::from_static(b"event: expired\ndata: -\n\n")).await;
                    break;
                }
                Bytes::from_static(b": keep-alive\n\n")
            }
            () = shutdown.cancelled() => break,
            () = tx.closed() => break,
        };
        if !send(bytes).await {
            break;
        }
    }
}

/// Sends `bytes` on the stream unless the server shuts down or the client
/// goes away first. Whether it was sent.
async fn deliver(tx: &Outgoing, shutdown: &CancellationToken, bytes: Bytes) -> bool {
    tokio::select! {
        sent = tx.send(Ok(bytes)) => sent.is_ok(),
        () = shutdown.cancelled() => false,
        () = tx.closed() => false,
    }
}

/// Whether the request's session is still valid (not expired or revoked).
async fn session_holds(state: &Arc<WebState>, request: &WebRequest) -> bool {
    let state = Arc::clone(state);
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        state
            .shared
            .store()
            .lock()
            .is_ok_and(|store| open_session(&store, &state, &request).is_ok())
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;

    const WINDOW: Duration = Duration::from_millis(1000);

    #[test]
    fn the_first_change_goes_out_at_once_and_a_burst_waits_for_the_window() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut c = Coalescer::new(Some("a".into()), WINDOW);
        assert_eq!(c.observe(Some("a".into()), at(0), 0), None, "no change");
        let first = c.observe(Some("b".into()), at(200), 200);
        assert_eq!(
            first,
            Some(Signal {
                generation: 1,
                seen_ms: 200
            })
        );
        assert_eq!(
            c.observe(Some("c".into()), at(400), 400),
            None,
            "inside the window"
        );
        assert_eq!(
            c.observe(Some("d".into()), at(600), 600),
            None,
            "still inside"
        );
        let second = c.observe(Some("d".into()), at(1200), 1200);
        assert_eq!(
            second,
            Some(Signal {
                generation: 2,
                seen_ms: 400
            }),
            "one signal for the burst, timed from its first change"
        );
        assert_eq!(c.observe(Some("d".into()), at(3000), 3000), None, "quiet");
        assert_eq!(
            c.observe(Some("e".into()), at(3200), 3200)
                .map(|s| s.generation),
            Some(3),
            "after a quiet window, at once again"
        );
    }

    #[tokio::test]
    async fn the_watcher_polls_only_while_a_stream_listens() {
        let reads = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&reads);
        let read: Arc<MarkerSource> = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Some("m".to_owned())
        });
        let signal = BoardSignal::new(read, WINDOW, CancellationToken::new());
        tokio::time::sleep(TICK * 3).await;
        assert_eq!(reads.load(Ordering::SeqCst), 0, "no stream, no polling");
        let (rx, stale) = signal.subscribe(Some("m".to_owned()));
        assert!(!stale);
        tokio::time::sleep(TICK * 3).await;
        assert!(reads.load(Ordering::SeqCst) > 0);
        let (second, stale) = signal.subscribe(Some("old".to_owned()));
        assert!(stale, "a page behind the watcher hears at once");
        drop((rx, second));
        tokio::time::sleep(TICK * 3).await;
        assert!(!signal.running());
        let after = reads.load(Ordering::SeqCst);
        tokio::time::sleep(TICK * 3).await;
        assert_eq!(reads.load(Ordering::SeqCst), after, "stopped");
    }

    /// Reads what a feed sent, until `needle` shows or `within` passes.
    async fn read_until(
        out: &mut mpsc::Receiver<Result<Bytes, std::convert::Infallible>>,
        needle: &str,
        within: Duration,
    ) -> String {
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + within;
        while !seen.contains(needle) {
            match tokio::time::timeout_at(deadline, out.recv()).await {
                Ok(Some(Ok(bytes))) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                _ => break,
            }
        }
        seen
    }

    /// JD-66 MED-04: a document stream subscribes, then reads the
    /// document's revision. A change published in between (the watcher
    /// announcing a write that lands just after the read) is still seen.
    #[tokio::test]
    async fn a_change_between_the_subscription_and_the_revision_read_is_seen() {
        let (publish, _keep) = watch::channel(Signal::default());
        let (rx, read) = subscribe_then_read(
            || publish.subscribe(),
            || async {
                // The read: revision 1, then the write lands and is published.
                publish.send_replace(Signal {
                    generation: 1,
                    seen_ms: 5,
                });
                1_u64
            },
        )
        .await;
        assert_eq!(read, 1);
        assert_eq!(
            rx.has_changed().ok(),
            Some(true),
            "the receiver sees the change, so the feed reads revision 2"
        );
        // The old order loses it: a receiver made after the publication
        // counts it as seen.
        let (publish, _keep) = watch::channel(Signal::default());
        publish.send_replace(Signal {
            generation: 1,
            seen_ms: 5,
        });
        let late = publish.subscribe();
        assert_eq!(late.has_changed().ok(), Some(false));
    }

    #[tokio::test]
    async fn a_signal_published_before_the_loop_starts_is_sent() {
        let (publish, rx) = watch::channel(Signal::default());
        // The watcher publishes after the stream subscribed, before its
        // loop runs.
        publish.send_replace(Signal {
            generation: 1,
            seen_ms: 5,
        });
        let (tx, mut out) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            run_feed(rx, false, tx, &stop, Duration::from_secs(15), || async {
                true
            })
            .await;
        });
        let seen = read_until(&mut out, "data: 1 5", Duration::from_secs(1)).await;
        assert!(seen.contains("id: 1\ndata: 1 5"), "{seen:?}");
        shutdown.cancel();
        task.await.unwrap();
        drop(publish);
    }

    #[tokio::test]
    async fn an_unread_stream_ends_at_shutdown_and_gives_back_its_place() {
        let read: Arc<MarkerSource> = Arc::new(|| None);
        let signal = BoardSignal::new(read, WINDOW, CancellationToken::new());
        let place = signal.try_open().unwrap();
        let (publish, rx) = watch::channel(Signal::default());
        // Room for the retry line only; nobody reads.
        let (tx, out) = mpsc::channel(1);
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            let _place = place;
            run_feed(rx, true, tx, &stop, Duration::from_secs(15), || async {
                true
            })
            .await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the stream ends within 1 s of shutdown")
            .unwrap();
        assert_eq!(
            signal.open.load(Ordering::Acquire),
            0,
            "its place is given back"
        );
        drop((publish, out));
    }

    #[tokio::test]
    async fn an_unread_stream_with_a_lapsed_session_ends_at_shutdown() {
        let (publish, rx) = watch::channel(Signal::default());
        let (tx, out) = mpsc::channel(1);
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            run_feed(rx, false, tx, &stop, Duration::from_millis(50), || async {
                false
            })
            .await;
        });
        // The retry line fills the channel; at the first keep-alive the
        // session has lapsed and `expired` cannot be delivered.
        tokio::time::sleep(Duration::from_millis(200)).await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the stream ends within 1 s of shutdown")
            .unwrap();
        drop((publish, out));
    }

    #[test]
    fn places_are_limited_and_given_back() {
        let read: Arc<MarkerSource> = Arc::new(|| None);
        let signal = BoardSignal::new(read, WINDOW, CancellationToken::new());
        let places: Vec<_> = (0..MAX_BOARD_STREAMS)
            .filter_map(|_| signal.try_open())
            .collect();
        assert_eq!(places.len(), MAX_BOARD_STREAMS);
        assert!(signal.try_open().is_none());
        drop(places);
        assert!(signal.try_open().is_some());
    }
}
