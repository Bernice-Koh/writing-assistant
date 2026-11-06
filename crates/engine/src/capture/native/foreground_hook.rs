//! Foreground changes for the capture thread, beside the focus handler. On 5 November 2026 a
//! Word document started from the shell and then clicked into three times raised no focus event
//! the capture thread's handler received, in 5 of 5 starts (#157), so the thread kept the element
//! it had scoped before. Word's window raised `EVENT_SYSTEM_FOREGROUND` twice as it started without
//! taking the foreground, and none when the click made it the foreground window: with the hook
//! alone the document was still not scoped in 5 of 5 starts.
//!
//! So this module watches the foreground two ways on a thread of its own, which pumps the messages
//! both arrive through: one out-of-context hook for `EVENT_SYSTEM_FOREGROUND`, and a read of the
//! foreground window every [`POLL`], which notices a change of window that no event announced. It
//! reports each change once [`GRACE`] has passed with no further change, and the capture thread
//! then re-resolves the focused element when no focus event followed the change
//! ([`reresolve_after`]). With both, the document was scoped after the first click in 5 of 5
//! starts, through the read.

use std::cell::Cell;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessageW, KillTimer, PeekMessageW,
    PostThreadMessageW, SetTimer, EVENT_SYSTEM_FOREGROUND, MSG, PM_NOREMOVE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_QUIT, WM_TIMER, WM_USER,
};

use super::error::NativeCaptureError;

/// How long after a foreground change the change is reported. A focus event that follows a
/// foreground change arrives within a few milliseconds of it, so a change still without one after
/// this is taken to have raised none.
const GRACE: Duration = Duration::from_millis(300);

/// How often the foreground window is read for a change no event announced. One call that reads
/// a handle, five times a second.
const POLL: Duration = Duration::from_millis(200);

/// Whether the capture thread re-resolves the focused element for a foreground change at
/// `changed_at`: when no focus signal has been handled since it, `last_focus_signal` being when
/// the last one was.
pub(super) fn reresolve_after(changed_at: Instant, last_focus_signal: Option<Instant>) -> bool {
    last_focus_signal.is_none_or(|focus| focus < changed_at)
}

/// Whether the foreground window read now, `now`, is a change from the one read last, `last`, by
/// handle. The first read, with nothing read before it, is no change.
fn foreground_moved(last: Option<Option<HWND>>, now: Option<HWND>) -> bool {
    last.is_some_and(|last| last != now)
}

/// The watching thread. Stopping it unregisters the hook and ends the thread.
pub(super) struct ForegroundHook {
    thread_id: u32,
    join: JoinHandle<()>,
}

impl ForegroundHook {
    /// Starts the watching thread, which calls `settled` with the time of each foreground change,
    /// [`GRACE`] after the last of a run of changes, and blocks until the hook is registered.
    pub(super) fn start(
        settled: impl Fn(Instant) + Send + 'static,
    ) -> Result<Self, NativeCaptureError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("writing-assistant-foreground-hook".to_owned())
            .spawn(move || run(&ready_tx, &settled))
            .map_err(|error| NativeCaptureError::ThreadSpawn(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, join }),
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

    /// Ends the thread's message loop, which unregisters the hook, and joins the thread.
    pub(super) fn stop(self) {
        // SAFETY: `thread_id` is the watching thread's, whose message queue exists from before it
        // signalled readiness; a thread that has already ended fails the call safely (Err).
        if let Err(error) = unsafe {
            PostThreadMessageW(
                self.thread_id,
                WM_QUIT,
                Default::default(),
                Default::default(),
            )
        } {
            log::debug!("could not post the quit message to the foreground hook's thread: {error}");
        }
        let _ = self.join.join();
    }
}

thread_local! {
    /// When the latest foreground change in the current run of changes happened.
    static CHANGED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The id of the grace timer, 0 when none is set.
    static GRACE_TIMER: Cell<usize> = const { Cell::new(0) };
    /// The foreground window the last poll read, `None` before the first poll.
    static LAST_FOREGROUND: Cell<Option<Option<HWND>>> = const { Cell::new(None) };
}

fn run(ready: &mpsc::Sender<Result<u32, NativeCaptureError>>, settled: &dyn Fn(Instant)) {
    let mut message = MSG::default();
    // SAFETY: `message` is a valid out parameter. Peeking creates the thread's message queue, so a
    // quit message posted after readiness is never lost.
    unsafe {
        let _ = PeekMessageW(&raw mut message, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    // SAFETY: `on_foreground` matches WINEVENTPROC and lives for the program; an out-of-context
    // hook is delivered through this thread's message loop below, on this thread.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_invalid() {
        let _ = ready.send(Err(NativeCaptureError::Com(
            windows::core::Error::from_thread(),
        )));
        return;
    }
    let poll = u32::try_from(POLL.as_millis()).expect("the poll interval is a few hundred ms");
    // SAFETY: a thread timer with no window and no callback, delivered as WM_TIMER to this
    // thread's queue until KillTimer, which the thread's end makes unnecessary.
    let poll_timer = unsafe { SetTimer(None, 0, poll, None) };
    // SAFETY: GetCurrentThreadId takes no arguments.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    loop {
        // SAFETY: `message` is a valid out parameter; GetMessageW answers 0 for WM_QUIT and -1 on
        // an error, both of which end the loop.
        if unsafe { GetMessageW(&raw mut message, None, 0, 0) }.0 <= 0 {
            break;
        }
        if message.message == WM_TIMER && message.wParam.0 == poll_timer {
            // SAFETY: GetForegroundWindow takes no arguments; a null handle is read as `None`.
            let hwnd = unsafe { GetForegroundWindow() };
            let now = (!hwnd.is_invalid()).then_some(hwnd);
            if foreground_moved(LAST_FOREGROUND.with(Cell::get), now) {
                log::debug!("the foreground window changed with no event, read by the poll");
                record_change();
            }
            LAST_FOREGROUND.with(|last| last.set(Some(now)));
            continue;
        }
        if message.message == WM_TIMER && message.wParam.0 == GRACE_TIMER.with(Cell::get) {
            // SAFETY: the timer id is the one SetTimer returned on this thread.
            let _ = unsafe { KillTimer(None, message.wParam.0) };
            GRACE_TIMER.with(|timer| timer.set(0));
            if let Some(changed_at) = CHANGED_AT.with(Cell::take) {
                settled(changed_at);
            }
            continue;
        }
        // SAFETY: `message` was just filled in by GetMessageW.
        unsafe { DispatchMessageW(&raw const message) };
    }
    // SAFETY: `hook` is the registration made above on this thread.
    let _ = unsafe { UnhookWinEvent(hook) };
}

/// Records a foreground change and sets, or resets, the grace timer, so a run of changes is
/// reported once, after its last.
fn record_change() {
    CHANGED_AT.with(|changed_at| changed_at.set(Some(Instant::now())));
    let grace = u32::try_from(GRACE.as_millis()).expect("the grace period is a few hundred ms");
    // SAFETY: a thread timer with no window and no callback, delivered as WM_TIMER to this
    // thread's queue; an id of a timer that still exists replaces it.
    let timer = unsafe { SetTimer(None, GRACE_TIMER.with(Cell::get), grace, None) };
    GRACE_TIMER.with(|current| current.set(timer));
}

/// The hook's callback, on the watching thread.
unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    log::debug!("the foreground window changed, raised by EVENT_SYSTEM_FOREGROUND");
    record_change();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(value: usize) -> Option<HWND> {
        Some(HWND(value as *mut core::ffi::c_void))
    }

    #[test]
    fn a_foreground_change_with_no_focus_signal_yet_is_re_resolved() {
        assert!(reresolve_after(Instant::now(), None));
    }

    #[test]
    fn a_foreground_change_after_the_last_focus_signal_is_re_resolved() {
        let focus = Instant::now();
        let changed_at = focus + Duration::from_millis(5);
        assert!(reresolve_after(changed_at, Some(focus)));
    }

    #[test]
    fn a_foreground_change_followed_by_a_focus_signal_is_left_to_it() {
        let changed_at = Instant::now();
        let focus = changed_at + Duration::from_millis(5);
        assert!(!reresolve_after(changed_at, Some(focus)));
    }

    #[test]
    fn a_focus_signal_at_the_change_itself_counts_as_following_it() {
        let at = Instant::now();
        assert!(!reresolve_after(at, Some(at)));
    }

    #[test]
    fn the_first_poll_is_no_change() {
        assert!(!foreground_moved(None, handle(1)));
    }

    #[test]
    fn the_same_window_in_front_is_no_change() {
        assert!(!foreground_moved(Some(handle(1)), handle(1)));
    }

    #[test]
    fn another_window_in_front_is_a_change() {
        assert!(foreground_moved(Some(handle(1)), handle(2)));
    }

    #[test]
    fn a_window_in_front_after_none_and_none_after_a_window_are_changes() {
        assert!(foreground_moved(Some(None), handle(1)));
        assert!(foreground_moved(Some(handle(1)), None));
    }
}
