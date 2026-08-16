//! Wires focus subscription, text-change notification, and cursor-rect retrieval together on
//! one dedicated UI Automation thread.
//!
//! UIA's COM interface types are `!Send`/`!Sync` (windows-rs marks them so deliberately: a
//! COM pointer is not generally safe to use from a thread other than the one that owns its
//! apartment membership). Both event handlers therefore only ever *signal* the owning thread
//! from whatever UIA-managed callback thread they're delivered on; every actual UIA call
//! (re-scoping the text-change registration, fetching the caret rect) happens back on the one
//! thread that owns the client, never inside a callback. The `Capture` trait's methods (#20)
//! extend the same principle: they too only send a signal and await a reply carrying owned
//! data, never a COM object, across the boundary.

pub mod client;
pub mod cursor;
pub mod error;
mod focus;
pub mod insert;
mod process;
mod text_change;

use std::sync::mpsc as ready_channel;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use tokio::sync::{mpsc as signal_channel, oneshot};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationEventHandler, IUIAutomationFocusChangedEventHandler,
};

use crate::capture::{CaptureError, CursorRect};
use client::Uia;
use error::NativeCaptureError;
use focus::FocusHandler;

/// A live capture session: one dedicated thread owning the UIA client, the focus-changed
/// registration, and whichever element the text-change registration currently targets.
/// Dropping it unregisters everything and joins the thread.
pub struct NativeCapture {
    tx: Option<signal_channel::UnboundedSender<Signal>>,
    join: Option<JoinHandle<()>>,
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
        let join = thread::Builder::new()
            .name("writing-assistant-uia".to_owned())
            .spawn(move || run(&ready_tx, rx, thread_tx))
            .map_err(|error| NativeCaptureError::ThreadSpawn(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx: Some(tx),
                join: Some(join),
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

    async fn cursor_rect(&self) -> Result<CursorRect, CaptureError> {
        self.request(Signal::GetCursorRect).await
    }

    async fn replace(
        &self,
        anchor: &str,
        local_start: usize,
        local_length: usize,
        replacement: &str,
    ) -> Result<(), CaptureError> {
        let anchor = anchor.to_owned();
        let replacement = replacement.to_owned();
        self.request(|reply| Signal::Replace {
            anchor,
            local_start,
            local_length,
            replacement,
            reply,
        })
        .await
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
    GetCursorRect(oneshot::Sender<Result<CursorRect, CaptureError>>),
    Replace {
        anchor: String,
        local_start: usize,
        local_length: usize,
        replacement: String,
        reply: oneshot::Sender<Result<(), CaptureError>>,
    },
}

impl Signal {
    fn name(&self) -> &'static str {
        match self {
            Signal::FocusChanged => "FocusChanged",
            Signal::TextChanged => "TextChanged",
            Signal::Stop => "Stop",
            Signal::GetText(_) => "GetText",
            Signal::GetCursorRect(_) => "GetCursorRect",
            Signal::Replace { .. } => "Replace",
        }
    }
}

/// The active text-change scope: which element it's registered against, and the handler COM
/// object UIA holds a reference to, so it can be removed by that exact pair before the next
/// scope is registered.
struct TextChangeScope {
    element: IUIAutomationElement,
    handler: IUIAutomationEventHandler,
}

fn run(
    ready: &ready_channel::Sender<Result<(), NativeCaptureError>>,
    mut rx: signal_channel::UnboundedReceiver<Signal>,
    tx: signal_channel::UnboundedSender<Signal>,
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

    while let Some(signal) = rx.blocking_recv() {
        log::debug!("signal received: {}", signal.name());
        match signal {
            Signal::Stop => break,
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
                log_caret_rect("focus changed", &element);
                let text_tx = tx.clone();
                let callback: text_change::TextChangeCallback =
                    Arc::new(move |_element: &IUIAutomationElement| {
                        let _ = text_tx.send(Signal::TextChanged);
                    });
                // SAFETY: `uia.client()`, `cache`, and `element` are all live and owned by this
                // thread.
                match unsafe { text_change::register(uia.client(), &cache, &element, callback) } {
                    Ok(handler) => {
                        current_scope = Some(TextChangeScope { element, handler });
                    }
                    Err(error) => {
                        log::debug!("failed to register text-change handler: {error}");
                    }
                }
            }
            Signal::TextChanged => {
                if let Some(scope) = &current_scope {
                    log_caret_rect("text changed", &scope.element);
                }
            }
            Signal::GetText(reply) => {
                let result = match &current_scope {
                    Some(scope) => insert::current_text(&scope.element).map_err(CaptureError::from),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::GetCursorRect(reply) => {
                let result = match &current_scope {
                    Some(scope) => cursor::caret_rect(&scope.element).map_err(CaptureError::from),
                    None => Err(CaptureError::NoFocus),
                };
                let _ = reply.send(result);
            }
            Signal::Replace {
                anchor,
                local_start,
                local_length,
                replacement,
                reply,
            } => {
                let result = match &current_scope {
                    Some(scope) => insert::replace_within(
                        &scope.element,
                        &anchor,
                        local_start,
                        local_length,
                        &replacement,
                    )
                    .map(|method| log::info!("replace succeeded via {method:?}"))
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
}

fn log_caret_rect(reason: &str, element: &IUIAutomationElement) {
    match cursor::caret_rect(element) {
        Ok(rect) => log::info!("{reason}: caret rect {rect:?} on {}", describe(element)),
        Err(error) => {
            log::debug!("{reason}: no caret rect ({error}) on {}", describe(element));
        }
    }
}

/// Identifies an element by class, control type, and owning process, so a caret-rect failure
/// can be traced to *which* element UIA handed over rather than only that it had no caret.
/// Deliberately omits the element's `Name`: on some controls that property carries the user's
/// own text, which never enters a log at any level.
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
