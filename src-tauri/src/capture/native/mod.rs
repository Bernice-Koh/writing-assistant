//! Wires focus subscription, text-change notification, and cursor-rect retrieval together on
//! one dedicated UI Automation thread.
//!
//! UIA's COM interface types are `!Send`/`!Sync` (windows-rs marks them so deliberately: a
//! COM pointer is not generally safe to use from a thread other than the one that owns its
//! apartment membership). Both event handlers therefore only ever *signal* the owning thread
//! from whatever UIA-managed callback thread they're delivered on; every actual UIA call
//! (re-scoping the text-change registration, fetching the caret rect) happens back on the one
//! thread that owns the client, never inside a callback.

pub mod client;
pub mod cursor;
pub mod error;
mod focus;
pub mod insert;
mod text_change;

use std::sync::mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationEventHandler, IUIAutomationFocusChangedEventHandler,
};

use client::Uia;
use error::NativeCaptureError;
use focus::FocusHandler;

/// A live capture session: one dedicated thread owning the UIA client, the focus-changed
/// registration, and whichever element the text-change registration currently targets.
/// Dropping it unregisters everything and joins the thread.
pub struct NativeCapture {
    stop: Option<mpsc::Sender<Signal>>,
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
        let (stop_tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_tx = stop_tx.clone();
        let join = thread::Builder::new()
            .name("writing-assistant-uia".to_owned())
            .spawn(move || run(&ready_tx, rx, thread_tx))
            .map_err(|error| NativeCaptureError::ThreadSpawn(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop: Some(stop_tx),
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
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(Signal::Stop);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Sent through one shared channel so the owning thread's loop can wait on a single receiver:
/// UIA delivers focus and text-change events on its own callback threads, and `Drop` signals
/// teardown from whatever thread drops the `NativeCapture`.
enum Signal {
    FocusChanged,
    TextChanged,
    Stop,
}

impl Signal {
    fn name(&self) -> &'static str {
        match self {
            Signal::FocusChanged => "FocusChanged",
            Signal::TextChanged => "TextChanged",
            Signal::Stop => "Stop",
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
    ready: &mpsc::Sender<Result<(), NativeCaptureError>>,
    rx: mpsc::Receiver<Signal>,
    tx: mpsc::Sender<Signal>,
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

    for signal in rx {
        log::debug!("signal received: {}", signal.name());
        match signal {
            Signal::Stop => break,
            Signal::FocusChanged => {
                if let Some(scope) = current_scope.take() {
                    // SAFETY: `scope.element`/`scope.handler` are the exact pair returned by
                    // the matching `text_change::register` call below.
                    if let Err(error) =
                        unsafe { text_change::remove(uia.client(), &scope.element, &scope.handler) }
                    {
                        log::debug!("failed to remove previous text-change registration: {error}");
                    }
                }
                match uia.focused_element(&cache) {
                    Ok(element) => {
                        log_caret_rect("focus changed", &element);
                        let text_tx = tx.clone();
                        let callback: text_change::TextChangeCallback =
                            Arc::new(move |_element: &IUIAutomationElement| {
                                let _ = text_tx.send(Signal::TextChanged);
                            });
                        // SAFETY: `uia.client()`, `cache`, and `element` are all live and owned
                        // by this thread.
                        match unsafe {
                            text_change::register(uia.client(), &cache, &element, callback)
                        } {
                            Ok(handler) => {
                                current_scope = Some(TextChangeScope { element, handler });
                            }
                            Err(error) => {
                                log::debug!("failed to register text-change handler: {error}");
                            }
                        }
                    }
                    Err(error) => log::debug!("no focused element: {error}"),
                }
            }
            Signal::TextChanged => {
                if let Some(scope) = &current_scope {
                    log_caret_rect("text changed", &scope.element);
                }
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
        Ok(rect) => log::info!("{reason}: caret rect {rect:?}"),
        Err(error) => log::debug!("{reason}: no caret rect ({error})"),
    }
}
