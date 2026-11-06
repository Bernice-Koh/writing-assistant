//! Wires focus subscription, text-change notification, and the reads of text and rectangles
//! together on one dedicated UI Automation thread.
//!
//! UIA's COM interface types are `!Send`/`!Sync` (windows-rs marks them so deliberately: a
//! COM pointer is not generally safe to use from a thread other than the one that owns its
//! apartment membership). Both event handlers therefore only ever *signal* the owning thread
//! from whatever UIA-managed callback thread they're delivered on; every actual UIA call
//! (re-scoping the text-change registration, reading a rectangle) happens back on the one
//! thread that owns the client, never inside a callback. The `Capture` trait's methods (#20)
//! extend the same principle: they too only send a signal and await a reply carrying owned
//! data, never a COM object, across the boundary.

pub mod client;
pub mod cursor;
pub mod error;
mod focus;
pub mod foreground;
mod foreground_hook;
pub mod insert;
mod process;
pub mod read;
pub mod surface;
mod text_change;

use std::sync::mpsc as ready_channel;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use tokio::sync::{mpsc as signal_channel, oneshot, watch};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationEventHandler,
    IUIAutomationFocusChangedEventHandler,
};

use crate::capture::{CaptureError, CursorRect, Foreground, Surface};
use crate::flag::Span;
use client::Uia;
use error::NativeCaptureError;
use focus::FocusHandler;
use foreground_hook::ForegroundHook;
use process::OwnProcesses;

/// A live capture session: one dedicated thread owning the UIA client, the focus-changed
/// registration, and whichever element the text-change registration currently targets.
/// Dropping it unregisters everything and joins the thread.
pub struct NativeCapture {
    tx: Option<signal_channel::UnboundedSender<Signal>>,
    join: Option<JoinHandle<()>>,
    /// The change notification, which the capture thread raises and [`Capture::changes`]
    /// subscribes to.
    ///
    /// [`Capture::changes`]: crate::capture::Capture::changes
    changes: Arc<watch::Sender<u64>>,
}

impl NativeCapture {
    /// Starts the capture thread and blocks until it has registered for focus-changed events
    /// (or failed to).
    ///
    /// # Errors
    /// Returns the error if the thread could not be spawned, ended before signalling
    /// readiness, or a UIA setup call failed.
    pub fn start() -> Result<Self, NativeCaptureError> {
        let (tx, rx) = signal_channel::unbounded_channel();
        let (ready_tx, ready_rx) = ready_channel::channel();
        let thread_tx = tx.clone();
        let changes = Arc::new(watch::channel(0).0);
        let thread_changes = Arc::clone(&changes);
        let join = thread::Builder::new()
            .name("writing-assistant-uia".to_owned())
            .spawn(move || run(&ready_tx, rx, thread_tx, &thread_changes))
            .map_err(|error| NativeCaptureError::ThreadSpawn(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx: Some(tx),
                join: Some(join),
                changes,
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(NativeCaptureError::ThreadNotReady)
            }
        }
    }

    /// Sends a signal built by `build` to the capture thread and awaits its reply. The bridge
    /// between this struct's async `Capture` methods and the thread's own synchronous,
    /// COM-apartment-bound event loop: `build` receives the reply half of a fresh channel and
    /// returns the `Signal` carrying it, so each caller only names which request it wants,
    /// never the channel plumbing itself.
    async fn request<T: Send>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, CaptureError>>) -> Signal,
    ) -> Result<T, CaptureError> {
        let tx = self.tx.as_ref().ok_or_else(|| {
            CaptureError::Communication("capture thread has already stopped".to_owned())
        })?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(build(reply_tx))
            .map_err(|_| CaptureError::Communication("capture thread is not running".to_owned()))?;
        reply_rx.await.map_err(|_| {
            CaptureError::Communication("capture thread dropped the reply".to_owned())
        })?
    }
}

#[async_trait::async_trait]
impl crate::capture::Capture for NativeCapture {
    async fn current_text(&self) -> Result<String, CaptureError> {
        self.request(Signal::GetText).await
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    async fn replace(&self, span: &Span, replacement: &str) -> Result<(), CaptureError> {
        let span = span.clone();
        let replacement = replacement.to_owned();
        self.request(|reply| Signal::Replace {
            span,
            replacement,
            reply,
        })
        .await
    }

    async fn document_view_rect(&self) -> Result<CursorRect, CaptureError> {
        self.request(Signal::GetDocumentViewRect).await
    }

    async fn foreground(&self) -> Result<Foreground, CaptureError> {
        self.request(Signal::GetForeground).await
    }

    async fn span_rect(&self, span: &Span) -> Result<Vec<CursorRect>, CaptureError> {
        let span = span.clone();
        self.request(|reply| Signal::GetSpanRects { span, reply })
            .await
    }

    async fn surface(&self) -> Result<Surface, CaptureError> {
        self.request(Signal::GetSurface).await
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Signal::Stop);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Sent through one shared channel so the owning thread's loop can wait on a single receiver:
/// UIA delivers focus and text-change events on its own callback threads, `Drop` signals
/// teardown from whatever thread drops the `NativeCapture`, and a `Capture` trait call signals
/// a request from whatever async task made it.
enum Signal {
    FocusChanged,
    TextChanged,
    Stop,
    GetText(oneshot::Sender<Result<String, CaptureError>>),
    Replace {
        span: Span,
        replacement: String,
        reply: oneshot::Sender<Result<(), CaptureError>>,
    },
    GetDocumentViewRect(oneshot::Sender<Result<CursorRect, CaptureError>>),
    GetForeground(oneshot::Sender<Result<Foreground, CaptureError>>),
    GetSurface(oneshot::Sender<Result<Surface, CaptureError>>),
    GetSpanRects {
        span: Span,
        reply: oneshot::Sender<Result<Vec<CursorRect>, CaptureError>>,
    },
    /// The foreground window changed at the given time, and the foreground watch's grace period
    /// has passed with no further change.
    ForegroundSettled(Instant),
}

impl Signal {
    fn name(&self) -> &'static str {
        match self {
            Signal::FocusChanged => "FocusChanged",
            Signal::TextChanged => "TextChanged",
            Signal::Stop => "Stop",
            Signal::GetText(_) => "GetText",
            Signal::Replace { .. } => "Replace",
            Signal::GetDocumentViewRect(_) => "GetDocumentViewRect",
            Signal::GetForeground(_) => "GetForeground",
            Signal::GetSurface(_) => "GetSurface",
            Signal::GetSpanRects { .. } => "GetSpanRects",
            Signal::ForegroundSettled(_) => "ForegroundSettled",
        }
    }
}

/// The active text-change scope: which element it's registered against, and the handler COM
/// object UIA holds a reference to, so it can be removed by that exact pair before the next
/// scope is registered.
struct TextChangeScope {
    element: IUIAutomationElement,
    handler: IUIAutomationEventHandler,
    /// The id of the process that owns `element`, against which the foreground window's is
    /// classified.
    process_id: u32,
    /// The executable name of `process_id`'s process, read from the process table at the scope's
    /// first surface read and kept, since the scope's process does not change.
    process_name: Option<String>,
    /// Whether a read of this scope's element has logged that it took more than one part. A new
    /// scope starts on every focus change to an element outside this app, so the line is once
    /// per focus, while the analyzer reads after every pause in typing and at its safety poll.
    multi_part_read_logged: bool,
}

/// Reads the whole text of `scope`'s element. The first read in the scope that took more than one
/// part logs the count and the length at `info`, so a log shows a document past a provider's cap;
/// the line never holds text.
fn read_scope_text(scope: &mut TextChangeScope) -> Result<String, NativeCaptureError> {
    let read = read::document_text(&scope.element)?;
    if read.parts > 1 && !scope.multi_part_read_logged {
        scope.multi_part_read_logged = true;
        log::info!(
            "read the focused document in {} parts, {} characters",
            read.parts,
            read.text.chars().count()
        );
    }
    Ok(read.text)
}

/// The surface of `scope`'s element, by [`surface::read`]. The process is named from the process
/// table once per scope; a table that names no such process answers an error, since the register
/// map cannot be read without the name.
fn read_scope_surface(
    client: &IUIAutomation,
    scope: &mut TextChangeScope,
) -> Result<Surface, CaptureError> {
    if scope.process_name.is_none() {
        scope.process_name = process::executable_name(scope.process_id);
    }
    let process_name = scope.process_name.clone().ok_or_else(|| {
        CaptureError::Communication(format!(
            "the process table names no process {}",
            scope.process_id
        ))
    })?;
    Ok(surface::read(client, &scope.element, process_name).surface)
}

/// Raises the change notification for every receiver of the backend's `changes`.
fn notify_change(changes: &watch::Sender<u64>) {
    changes.send_modify(|count| *count = count.wrapping_add(1));
}

fn run(
    ready: &ready_channel::Sender<Result<(), NativeCaptureError>>,
    mut rx: signal_channel::UnboundedReceiver<Signal>,
    tx: signal_channel::UnboundedSender<Signal>,
    changes: &watch::Sender<u64>,
) {
    let setup = (|| -> Result<_, NativeCaptureError> {
        let uia = Uia::new()?;
        let cache = uia.base_cache_request()?;
        let focus_tx = tx.clone();
        let focus_handler: IUIAutomationFocusChangedEventHandler = FocusHandler {
            callback: Arc::new(move |_element: &IUIAutomationElement| {
                let _ = focus_tx.send(Signal::FocusChanged);
            }),
        }
        .into();
        // SAFETY: `cache` and `focus_handler` are live; the client is this thread's own.
        unsafe {
            uia.client()
                .AddFocusChangedEventHandler(&cache, &focus_handler)?
        };
        Ok((uia, cache, focus_handler))
    })();

    let (uia, cache, focus_handler) = match setup {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    // Seed initial state: focus-changed only fires on a *change*, so whatever already has
    // focus when this thread starts up needs one explicit kick to be tracked.
    let _ = tx.send(Signal::FocusChanged);

    let mut current_scope: Option<TextChangeScope> = None;
    let mut own_processes = OwnProcesses::default();
    let hook_tx = tx.clone();
    let foreground_hook = ForegroundHook::start(move |changed_at| {
        let _ = hook_tx.send(Signal::ForegroundSettled(changed_at));
    })
    .map_err(|error| {
        log::warn!("no foreground-change hook, so focus follows focus events alone: {error}");
    })
    .ok();
    let mut last_focus_signal: Option<Instant> = None;

    while let Some(signal) = rx.blocking_recv() {
        log::debug!("signal received: {}", signal.name());
        if matches!(signal, Signal::FocusChanged) {
            last_focus_signal = Some(Instant::now());
        }
        match signal {
            Signal::Stop => break,
            // A foreground change that no focus event followed leaves the scope on the element
            // focused before it; re-resolving the focused element is the focus handler's own work.
            Signal::ForegroundSettled(changed_at) => {
                if foreground_hook::reresolve_after(changed_at, last_focus_signal) {
                    log::info!(
                        "the foreground window changed and no focus event followed; re-resolving \
                         the focused element"
                    );
                    let _ = tx.send(Signal::FocusChanged);
                }
            }
            Signal::FocusChanged => {
                let element = match uia.focused_element(&cache) {
                    Ok(element) => element,
                    Err(error) => {
                        log::debug!("no focused element: {error}");
                        continue;
                    }
                };
                // SAFETY: `element` is live and owned by this thread.
                let pid = match unsafe { element.CurrentProcessId() } {
                    Ok(pid) => pid.cast_unsigned(),
                    Err(error) => {
                        log::debug!(
                            "could not read the focused element's process id, \
                             treating it as the user's: {error}"
                        );
                        0
                    }
                };
                // Resolved before the previous scope is torn down, so that clicking the app's
                // own window leaves whatever the user was writing in still tracked rather than
                // dropping it in favour of our own UI.
                if process::belongs_to_this_app(pid) {
                    log::debug!("focus is on this app's own window (pid {pid}); scope unchanged");
                    continue;
                }
                if let Some(scope) = current_scope.take() {
                    // SAFETY: `scope.element`/`scope.handler` are the exact pair returned by
                    // the matching `text_change::register` call below.
                    if let Err(error) =
                        unsafe { text_change::remove(uia.client(), &scope.element, &scope.handler) }
                    {
                        log::debug!("failed to remove previous text-change registration: {error}");
                    }
                }
                log::debug!("focus changed to {}", describe(&element));
                let text_tx = tx.clone();
                let callback: text_change::TextChangeCallback =
                    Arc::new(move |_element: &IUIAutomationElement| {
                        let _ = text_tx.send(Signal::TextChanged);
                    });
                // SAFETY: `uia.client()`, `cache`, and `element` are all live and owned by this
                // thread.
                match unsafe { text_change::register(uia.client(), &cache, &element, callback) } {
                    Ok(handler) => {
                        current_scope = Some(TextChangeScope {
                            element,
                            handler,
                            process_id: pid,
                            process_name: None,
                            multi_part_read_logged: false,
                        });
                        // A new element is a new document to check.
                        notify_change(changes);
                    }
                    Err(error) => {
                        log::debug!("failed to register text-change handler: {error}");
                    }
                }
            }
            Signal::TextChanged => notify_change(changes),
            Signal::GetText(reply) => {
                let result = match &mut current_scope {
                    Some(scope) => read_scope_text(scope).map_err(CaptureError::from),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::Replace {
                span,
                replacement,
                reply,
            } => {
                let result = match &current_scope {
                    Some(scope) => insert::replace_within(
                        &scope.element,
                        foreground::target_window(uia.client(), &scope.element),
                        &span.anchor,
                        span.occurrence,
                        span.local_start,
                        span.local_length,
                        &replacement,
                    )
                    .map(|method| log::info!("replace succeeded via {method:?}"))
                    .map_err(CaptureError::from),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::GetDocumentViewRect(reply) => {
                let result = match &current_scope {
                    Some(scope) => {
                        cursor::document_view_rect(&scope.element, scope.process_id, |process| {
                            own_processes.contains(process)
                        })
                        .map_err(CaptureError::from)
                    }
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::GetForeground(reply) => {
                let result = match &current_scope {
                    Some(scope) => Ok(foreground::classify(
                        foreground::window().map(|window| window.process_id),
                        scope.process_id,
                        |process| own_processes.contains(process),
                    )),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::GetSurface(reply) => {
                let result = match &mut current_scope {
                    Some(scope) => read_scope_surface(uia.client(), scope),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::GetSpanRects { span, reply } => {
                let result = match &current_scope {
                    Some(scope) => insert::select::find_within_range(
                        &scope.element,
                        &span.anchor,
                        span.occurrence,
                        span.local_start,
                        span.local_length,
                    )
                    .and_then(|range| cursor::span_rects(&range))
                    .map_err(CaptureError::from),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
        }
    }

    // SAFETY: unregistering this thread's own registrations before teardown, using the exact
    // handler objects that were registered above (UIA matches registrations by COM identity, so
    // a freshly constructed handler would not remove the real one).
    unsafe {
        let _ = uia.client().RemoveFocusChangedEventHandler(&focus_handler);
        if let Some(scope) = current_scope.take() {
            let _ = text_change::remove(uia.client(), &scope.element, &scope.handler);
        }
    }
    if let Some(hook) = foreground_hook {
        hook.stop();
    }
}

/// Identifies an element by class, control type, and owning process, so a focus change can be
/// traced to *which* element UIA handed over. Deliberately omits the element's `Name`: on some
/// controls that property carries the user's own text, which never enters a log at any level.
fn describe(element: &IUIAutomationElement) -> String {
    // SAFETY: `element` is live and owned by this thread; each property getter fails safely
    // (Err) rather than trapping, and every BSTR returned is dropped by windows-rs.
    unsafe {
        let class = element
            .CurrentClassName()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let control_type = element.CurrentControlType().map_or(0, |id| id.0);
        let pid = element.CurrentProcessId().unwrap_or(0);
        format!("class={class:?} control_type={control_type} pid={pid}")
    }
}
