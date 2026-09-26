//! Session-scoped low-level input hooks. The hook thread only copies input,
//! maintains gesture ownership, and posts bounded/coalesced notifications.

#[path = "input_protocol.rs"]
mod protocol;

use std::{
    cell::RefCell,
    marker::PhantomData,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            E_FAIL, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WAIT_FAILED, WAIT_TIMEOUT, WPARAM,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateEventW, INFINITE, SetEvent},
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1,
                VK_XBUTTON2,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{Error, Result},
};

use crate::core::{geometry::ScreenPointPx, state::SessionId};
use protocol::{Button, Phase, Protocol, Signal};

pub const WM_INPUT_WAKE: u32 = WM_APP + 10;
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum InputFailureKind {
    GestureAlreadyHeld = 1,
    HookInstall,
    EventQueueFull,
    ReceiverDisconnected,
    WakeFailed,
    ControlWakeFailed,
    MessageLoop,
    DrainTimeout,
    HookUninstall,
    ThreadPanicked,
    DpiContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputFailure {
    pub kind: InputFailureKind,
    pub code: i32,
}

impl std::fmt::Display for InputFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.kind {
            InputFailureKind::GestureAlreadyHeld => crate::app::i18n::tr(
                "请先松开鼠标按钮和 Esc，再开始取色",
                "Release the mouse buttons and Esc before picking",
            ),
            InputFailureKind::HookInstall => crate::app::i18n::tr(
                "无法安装本次会话的输入钩子",
                "Could not start input handling for this pick",
            ),
            InputFailureKind::EventQueueFull => crate::app::i18n::tr(
                "输入事件队列已满，本次取色已取消",
                "The input queue is full; picking was canceled",
            ),
            InputFailureKind::ReceiverDisconnected => crate::app::i18n::tr(
                "输入事件接收端已关闭",
                "The input event receiver has closed",
            ),
            InputFailureKind::WakeFailed => {
                crate::app::i18n::tr("无法唤醒取色窗口", "Could not wake the picker window")
            }
            InputFailureKind::ControlWakeFailed => crate::app::i18n::tr(
                "无法唤醒输入线程进行清理",
                "Could not wake the input thread for cleanup",
            ),
            InputFailureKind::MessageLoop => crate::app::i18n::tr(
                "输入线程消息等待失败",
                "The input thread could not wait for messages",
            ),
            InputFailureKind::DrainTimeout => crate::app::i18n::tr(
                "等待已按下的按键释放超时，本次取色已取消",
                "Timed out waiting for held keys to be released; picking was canceled",
            ),
            InputFailureKind::HookUninstall => {
                crate::app::i18n::tr("输入钩子卸载失败", "Could not remove the input hooks")
            }
            InputFailureKind::ThreadPanicked => {
                crate::app::i18n::tr("输入线程异常退出", "The input thread exited unexpectedly")
            }
            InputFailureKind::DpiContext => crate::app::i18n::tr(
                "输入线程未使用 PerMonitorV2 DPI 上下文",
                "The input thread is not using PerMonitorV2 DPI awareness",
            ),
        };
        write!(formatter, "{reason} (0x{:08X})", self.code as u32)
    }
}
impl std::error::Error for InputFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Ready {
        session: SessionId,
    },
    Candidate {
        session: SessionId,
        point: ScreenPointPx,
    },
    Cancel {
        session: SessionId,
    },
    Wheel {
        session: SessionId,
        point: ScreenPointPx,
        delta: i16,
    },
    Failed {
        session: SessionId,
        error: InputFailure,
    },
    Stopped {
        session: SessionId,
    },
}

pub struct InputSession {
    shared: Arc<Shared>,
    receiver: Receiver<InputEvent>,
    worker: Option<JoinHandle<std::result::Result<(), InputFailure>>>,
    _ui_thread: PhantomData<Rc<()>>,
}

impl InputSession {
    pub fn start(session: SessionId, notify: HWND) -> Result<Self> {
        if held_gesture() {
            return Err(Error::new(
                E_FAIL,
                crate::app::i18n::tr(
                    "请先松开鼠标按钮和 Esc，再开始取色",
                    "Release the mouse buttons and Esc before picking",
                ),
            ));
        }
        let signal = ControlSignal::new()?;
        let shared = Arc::new(Shared {
            session,
            notify: notify.0 as usize,
            control: signal,
            finish: AtomicBool::new(false),
            reject: AtomicBool::new(false),
            failure: AtomicU64::new(0),
            movement: MovementMailbox::default(),
            record_movement: crate::app::diagnostics::enabled(),
            movement_events: AtomicU64::new(0),
            movement_wakes: AtomicU64::new(0),
        });
        let (sender, receiver) = sync_channel(64);
        let thread_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("color-picker-input".into())
            .spawn(move || {
                let result = run_input(thread_shared.clone(), sender.clone());
                if let Err(error) = result {
                    thread_shared.fail(error);
                }
                if let Some(error) = thread_shared.failure() {
                    thread_shared.emit(&sender, InputEvent::Failed { session, error });
                }
                thread_shared.emit(&sender, InputEvent::Stopped { session });
                thread_shared.failure().map_or(Ok(()), Err)
            })
            .map_err(|error| {
                Error::new(
                    E_FAIL,
                    crate::tr_format!(
                        "无法创建输入线程：{error}",
                        "Could not create the input thread: {error}"
                    ),
                )
            })?;
        Ok(Self {
            shared,
            receiver,
            worker: Some(worker),
            _ui_thread: PhantomData,
        })
    }

    pub fn session_id(&self) -> SessionId {
        self.shared.session
    }
    pub fn drain_events(&mut self) -> Vec<InputEvent> {
        self.receiver.try_iter().take(64).collect()
    }
    pub fn take_movement(&self) -> Option<ScreenPointPx> {
        self.shared.movement.take()
    }
    pub fn set_movement_notifications(&self, enabled: bool) {
        self.shared.movement.set_enabled(enabled);
    }
    pub fn movement_counts(&self) -> (u64, u64) {
        (
            self.shared.movement_events.load(Ordering::Relaxed),
            self.shared.movement_wakes.load(Ordering::Relaxed),
        )
    }
    pub fn reject_candidate(&self) -> Result<()> {
        self.shared.reject.store(true, Ordering::Release);
        self.shared.signal_control()
    }
    pub fn request_finish(&self) -> Result<()> {
        self.set_movement_notifications(false);
        self.shared.finish.store(true, Ordering::Release);
        self.shared.signal_control()
    }
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Borrow the worker's waitable handle until the next mutable session use.
    /// Waiting alongside the UI queue requires no polling timer or duplication.
    pub fn wait_handle(&self) -> Option<BorrowedHandle<'_>> {
        self.worker.as_ref().map(AsHandle::as_handle)
    }
    pub fn try_join(&mut self) -> Option<std::result::Result<(), InputFailure>> {
        if !self.is_finished() {
            return None;
        }
        let worker = self.worker.take()?;
        Some(worker.join().unwrap_or_else(|_| {
            let error = failure(InputFailureKind::ThreadPanicked, 0);
            self.shared.fail(error);
            Err(error)
        }))
    }
    pub fn failure(&self) -> Option<InputFailure> {
        self.shared.failure()
    }
}

impl Drop for InputSession {
    fn drop(&mut self) {
        // Normal owners poll/join before dropping. Exceptional unwinding/early
        // return still requests bounded draining; never synchronously join here.
        let _ = self.request_finish();
        if self.is_finished() {
            let _ = self.try_join();
        }
    }
}

// OwnedHandle carries the unique CloseHandle responsibility and the standard
// library's cross-thread kernel-handle contract; HWND/GDI types stay thread bound.
struct ControlSignal(OwnedHandle);
impl ControlSignal {
    fn new() -> Result<Self> {
        let handle = create_control_event()?;
        // SAFETY: CreateEventW returned one new, valid event handle. No other
        // owner adopts it, and OwnedHandle uses its required CloseHandle release.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
    }
    fn handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
    fn set(&self) -> Result<()> {
        // SAFETY: The event borrow remains live through this non-retaining call;
        // SetEvent supports signaling this kernel object from another thread.
        unsafe { SetEvent(HANDLE(self.handle().as_raw_handle())) }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_EVENT_CREATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn create_control_event() -> Result<HANDLE> {
    #[cfg(test)]
    if FAIL_EVENT_CREATION.replace(false) {
        return Err(Error::new(
            E_FAIL,
            "injected control-event creation failure",
        ));
    }
    // SAFETY: This unnamed auto-reset event has no borrowed security descriptor
    // or name. A successful call returns a fresh uniquely owned kernel handle.
    unsafe { CreateEventW(None, false, false, None) }
}

#[cfg(test)]
mod handle_tests {
    use super::*;
    use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

    #[test]
    fn control_event_creation_failure_does_not_install_an_owner() {
        FAIL_EVENT_CREATION.set(true);
        assert!(ControlSignal::new().is_err());
        assert!(!FAIL_EVENT_CREATION.get());
        let event = ControlSignal::new().expect("failure injection must be local and one-shot");
        event.set().unwrap();
        let borrowed = event.handle();
        // SAFETY: This live borrow denotes the newly signaled event; the bounded
        // wait neither closes nor retains it beyond its owner.
        assert_eq!(
            // SAFETY: event owns this borrowed, signaled handle for the wait.
            unsafe { WaitForSingleObject(HANDLE(borrowed.as_raw_handle()), 1000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn control_event_can_be_signaled_across_threads_with_one_owner() {
        let signal = Arc::new(ControlSignal::new().unwrap());
        let released = Arc::downgrade(&signal);
        let producer = signal.clone();
        let worker = thread::spawn(move || producer.set().unwrap());
        {
            let borrowed = signal.handle();
            // SAFETY: The Arc retains the event throughout this bounded wait,
            // including while the other thread signals its own shared borrow.
            assert_eq!(
                // SAFETY: signal's Arc retains the event while this thread waits.
                unsafe { WaitForSingleObject(HANDLE(borrowed.as_raw_handle()), 5000) },
                WAIT_OBJECT_0
            );
        }
        worker.join().unwrap();
        assert_eq!(Arc::strong_count(&signal), 1);
        drop(signal);
        assert!(
            released.upgrade().is_none(),
            "the unique event owner must be released"
        );
    }

    fn session(worker: JoinHandle<std::result::Result<(), InputFailure>>) -> InputSession {
        let (_, receiver) = sync_channel(64);
        InputSession {
            shared: Arc::new(Shared {
                session: SessionId(1),
                notify: 0,
                control: ControlSignal::new().unwrap(),
                finish: AtomicBool::new(false),
                reject: AtomicBool::new(false),
                failure: AtomicU64::new(0),
                movement: MovementMailbox::default(),
                record_movement: false,
                movement_events: AtomicU64::new(0),
                movement_wakes: AtomicU64::new(0),
            }),
            receiver,
            worker: Some(worker),
            _ui_thread: PhantomData,
        }
    }

    fn wait_for_worker(session: &InputSession) {
        let borrowed = session.wait_handle().expect("worker is owned until join");
        // SAFETY: The immutable session borrow retains the JoinHandle during
        // this bounded kernel wait. No hook or desktop input is involved.
        assert_eq!(
            // SAFETY: The immutable session borrow keeps its thread handle live.
            unsafe { WaitForSingleObject(HANDLE(borrowed.as_raw_handle()), 5000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn borrowed_worker_handle_ends_before_successful_join() {
        let mut session = session(thread::spawn(|| Ok(())));
        wait_for_worker(&session);
        assert!(session.try_join().unwrap().is_ok());
        assert!(session.wait_handle().is_none());
        assert!(session.try_join().is_none());
    }

    #[test]
    fn borrowed_worker_handle_ends_before_panicked_worker_join() {
        let mut session = session(thread::spawn(|| {
            panic!("test worker failure without hooks")
        }));
        wait_for_worker(&session);
        assert_eq!(
            session.try_join().unwrap().unwrap_err().kind,
            InputFailureKind::ThreadPanicked
        );
        assert!(session.wait_handle().is_none());
        assert_eq!(
            session.failure().unwrap().kind,
            InputFailureKind::ThreadPanicked
        );
    }
}

struct Shared {
    session: SessionId,
    notify: usize,
    control: ControlSignal,
    finish: AtomicBool,
    reject: AtomicBool,
    failure: AtomicU64,
    movement: MovementMailbox,
    record_movement: bool,
    movement_events: AtomicU64,
    movement_wakes: AtomicU64,
}

impl Shared {
    fn fail(&self, error: InputFailure) {
        let packed = (u64::from(error.code as u32) << 32) | error.kind as u64;
        let _ = self
            .failure
            .compare_exchange(0, packed, Ordering::AcqRel, Ordering::Acquire);
        self.finish.store(true, Ordering::Release);
        let _ = self.control.set();
    }
    fn failure(&self) -> Option<InputFailure> {
        let packed = self.failure.load(Ordering::Acquire);
        let kind = match packed as u32 {
            0 => return None,
            1 => InputFailureKind::GestureAlreadyHeld,
            2 => InputFailureKind::HookInstall,
            3 => InputFailureKind::EventQueueFull,
            4 => InputFailureKind::ReceiverDisconnected,
            5 => InputFailureKind::WakeFailed,
            6 => InputFailureKind::ControlWakeFailed,
            7 => InputFailureKind::MessageLoop,
            8 => InputFailureKind::DrainTimeout,
            9 => InputFailureKind::HookUninstall,
            11 => InputFailureKind::DpiContext,
            _ => InputFailureKind::ThreadPanicked,
        };
        Some(InputFailure {
            kind,
            code: (packed >> 32) as u32 as i32,
        })
    }
    fn signal_control(&self) -> Result<()> {
        if let Err(error) = self.control.set() {
            self.fail(failure(InputFailureKind::ControlWakeFailed, error.code().0));
            return Err(error);
        }
        Ok(())
    }
    fn notify(&self) {
        // SAFETY: The UI host owns this HWND while the session drains; this
        // queued private message carries values only, never borrowed pointers.
        if let Err(error) = unsafe {
            PostMessageW(
                Some(HWND(self.notify as *mut _)),
                WM_INPUT_WAKE,
                WPARAM(self.session.0 as usize),
                LPARAM(0),
            )
        } {
            self.fail(failure(InputFailureKind::WakeFailed, error.code().0));
        }
    }
    fn emit(&self, sender: &SyncSender<InputEvent>, event: InputEvent) {
        match sender.try_send(event) {
            Ok(()) => self.notify(),
            Err(TrySendError::Full(_)) => self.fail(failure(InputFailureKind::EventQueueFull, 0)),
            Err(TrySendError::Disconnected(_)) => {
                self.fail(failure(InputFailureKind::ReceiverDisconnected, 0))
            }
        }
    }
    fn moved(&self, point: ScreenPointPx) {
        if self.record_movement {
            self.movement_events.fetch_add(1, Ordering::Relaxed);
        }
        if self.movement.publish(point) {
            if self.record_movement {
                self.movement_wakes.fetch_add(1, Ordering::Relaxed);
            }
            self.notify();
        }
    }
}

// One hook-thread producer and one UI-thread consumer. The UI alone changes
// enabled state. Combining enabled/pending with an epoch prevents a producer
// paused during a mode switch from setting pending in the next Frozen period.
#[derive(Default)]
struct MovementMailbox {
    state: AtomicU64,
    point: AtomicU64,
}

impl MovementMailbox {
    const ENABLED: u64 = 1;
    const PENDING: u64 = 2;
    const FLAGS: u64 = Self::ENABLED | Self::PENDING;

    fn set_enabled(&self, enabled: bool) {
        let epoch = (self.state.load(Ordering::Acquire) & !Self::FLAGS).wrapping_add(4);
        self.state
            .store(epoch | u64::from(enabled), Ordering::Release);
    }

    fn publish(&self, point: ScreenPointPx) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & Self::ENABLED == 0 {
            return false;
        }
        self.publish_in_epoch(point, state)
    }

    fn publish_in_epoch(&self, point: ScreenPointPx, mut state: u64) -> bool {
        let packed = u64::from(point.x as u32) | (u64::from(point.y as u32) << 32);
        self.point.store(packed, Ordering::Release);
        let epoch = state & !Self::FLAGS;
        loop {
            match self.state.compare_exchange_weak(
                state,
                state | Self::PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return state & Self::PENDING == 0,
                Err(current) if current & Self::ENABLED != 0 && current & !Self::FLAGS == epoch => {
                    // A consumer may have cleared pending after our point was
                    // stored. Retry in the same epoch to retain its next wake.
                    state = current;
                }
                Err(_) => return false,
            }
        }
    }

    fn take(&self) -> Option<ScreenPointPx> {
        // Clear before reading so a racing producer always retains a wakeup.
        let state = self.state.fetch_and(!Self::PENDING, Ordering::AcqRel);
        if state & Self::FLAGS != Self::FLAGS {
            return None;
        }
        let packed = self.point.load(Ordering::Acquire);
        Some(ScreenPointPx {
            x: packed as u32 as i32,
            y: (packed >> 32) as u32 as i32,
        })
    }
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn live_movement_never_wakes_and_frozen_coalesces_the_latest_point() {
        let mailbox = MovementMailbox::default();
        for x in 0..10_000 {
            assert!(!mailbox.publish(ScreenPointPx { x, y: -500 }));
        }
        assert_eq!(mailbox.take(), None);
        mailbox.set_enabled(true);
        assert!(mailbox.publish(ScreenPointPx { x: -1920, y: -1080 }));
        assert!(!mailbox.publish(ScreenPointPx { x: -10, y: 500 }));
        assert_eq!(mailbox.take(), Some(ScreenPointPx { x: -10, y: 500 }));
        assert_eq!(mailbox.take(), None);
        assert!(mailbox.publish(ScreenPointPx { x: 42, y: 63 }));
    }

    #[test]
    fn a_mode_switch_discards_pending_and_rejects_an_inflight_old_epoch() {
        let mailbox = MovementMailbox::default();
        mailbox.set_enabled(true);
        assert!(mailbox.publish(ScreenPointPx { x: 1, y: 1 }));
        let old_state = mailbox.state.load(Ordering::Acquire);
        mailbox.set_enabled(false);
        assert_eq!(mailbox.take(), None);
        assert!(!mailbox.publish(ScreenPointPx { x: 2, y: 2 }));
        mailbox.set_enabled(true);
        assert!(!mailbox.publish_in_epoch(ScreenPointPx { x: 3, y: 3 }, old_state));
        assert_eq!(mailbox.take(), None);
        assert!(mailbox.publish(ScreenPointPx { x: 4, y: 4 }));
        assert_eq!(mailbox.take(), Some(ScreenPointPx { x: 4, y: 4 }));
    }

    #[test]
    fn a_consumer_racing_a_coalesced_move_retains_a_new_wake() {
        let mailbox = MovementMailbox::default();
        mailbox.set_enabled(true);
        let first = ScreenPointPx { x: 1, y: -1 };
        let last = ScreenPointPx { x: 2, y: -2 };
        assert!(mailbox.publish(first));
        let producer_state = mailbox.state.load(Ordering::Acquire);
        assert_eq!(mailbox.take(), Some(first));
        // The producer started while pending was set, then the UI cleared it.
        assert!(mailbox.publish_in_epoch(last, producer_state));
        assert_eq!(mailbox.take(), Some(last));
    }

    #[test]
    fn movement_during_repeated_mode_switches_does_not_suppress_the_next_move() {
        let mailbox = Arc::new(MovementMailbox::default());
        let producer_mailbox = mailbox.clone();
        let producer = std::thread::spawn(move || {
            for x in 0..10_000 {
                producer_mailbox.publish(ScreenPointPx { x, y: -x });
            }
        });
        for _ in 0..1_000 {
            mailbox.set_enabled(true);
            mailbox.take();
            mailbox.set_enabled(false);
            assert_eq!(mailbox.take(), None);
        }
        producer.join().unwrap();
        mailbox.set_enabled(true);
        let last = ScreenPointPx {
            x: i32::MIN,
            y: i32::MAX,
        };
        assert!(mailbox.publish(last));
        assert_eq!(mailbox.take(), Some(last));
    }
}

struct HookContext {
    shared: Arc<Shared>,
    sender: SyncSender<InputEvent>,
    protocol: Protocol,
}
thread_local! { static CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) }; }

fn run_input(
    shared: Arc<Shared>,
    sender: SyncSender<InputEvent>,
) -> std::result::Result<(), InputFailure> {
    if shared.finish.load(Ordering::Acquire) {
        return Ok(());
    }
    super::check_environment()
        .map_err(|error| failure(InputFailureKind::DpiContext, error.code().0))?;
    if held_gesture() {
        return Err(failure(InputFailureKind::GestureAlreadyHeld, 0));
    }
    CONTEXT.with(|slot| {
        *slot.borrow_mut() = Some(HookContext {
            shared: shared.clone(),
            sender: sender.clone(),
            protocol: Protocol::new(),
        })
    });
    let _context_guard = ContextGuard;
    let mut hooks = HookGuards::default();
    // SAFETY: None requests the process module; this borrowed module is never
    // freed and contains both static hook callbacks for the process lifetime.
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| failure(InputFailureKind::HookInstall, error.code().0))?;
    hooks.mouse = Some(
        // SAFETY: The callback has the low-level mouse ABI, lives in this module,
        // and reads thread-local context retained until both hooks are removed.
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(HINSTANCE(module.0)), 0) }
            .map_err(|error| failure(InputFailureKind::HookInstall, error.code().0))?,
    );
    hooks.keyboard = Some(
        // SAFETY: The static low-level keyboard callback and module remain live;
        // HookGuards removes this hook before ContextGuard clears its context.
        unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                Some(HINSTANCE(module.0)),
                0,
            )
        }
        .map_err(|error| failure(InputFailureKind::HookInstall, error.code().0))?,
    );
    // Close the startup check/install gap. Any down already consumed by these
    // hooks remains tracked and will still be drained on this failure path.
    if held_gesture() {
        shared.fail(failure(InputFailureKind::GestureAlreadyHeld, 0));
    }
    if shared.failure().is_none() && !shared.finish.load(Ordering::Acquire) {
        // Until both hooks are installed and the final held-input check passes,
        // callbacks only pass input through. A partial install failure therefore
        // cannot leave a down consumed by a hook that is immediately removed.
        CONTEXT.with(|slot| {
            if let Some(context) = slot.borrow_mut().as_mut() {
                context.protocol.arm();
            }
        });
        shared.emit(
            &sender,
            InputEvent::Ready {
                session: shared.session,
            },
        );
    }
    let mut deadline = None;
    loop {
        let phase = CONTEXT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let context = slot
                .as_mut()
                .expect("input context exists while hooks are installed");
            if shared.finish.load(Ordering::Acquire) {
                context.protocol.finish();
            }
            if shared.reject.swap(false, Ordering::AcqRel) {
                context.protocol.reject();
            }
            context.protocol.phase()
        });
        if phase == Phase::Stopped {
            break;
        }
        let timeout = if phase == Phase::Draining {
            let until = *deadline.get_or_insert_with(|| Instant::now() + DRAIN_LIMIT);
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                shared.fail(failure(InputFailureKind::DrainTimeout, 0));
                break;
            }
            remaining.as_millis().max(1) as u32
        } else {
            INFINITE
        };
        let signal = shared.control.handle();
        let handles = [HANDLE(signal.as_raw_handle())];
        // SAFETY: Shared owns the borrowed event through this wait; the array is
        // local and neither the wait nor the message queue takes its ownership.
        let waited = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&handles), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if waited == WAIT_FAILED {
            shared.fail(failure(
                InputFailureKind::MessageLoop,
                Error::from_thread().code().0,
            ));
            break;
        }
        if waited == WAIT_TIMEOUT {
            shared.fail(failure(InputFailureKind::DrainTimeout, 0));
            break;
        }
        // PeekMessage also dispatches the sent messages used by low-level hooks.
        // Limit posted-message work so control/drain checks cannot be starved.
        for _ in 0..64 {
            let mut message = MSG::default();
            // SAFETY: The output MSG is initialized and local. No RefCell borrow
            // is held while USER32 may synchronously invoke our hook callbacks.
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                break;
            }
            if message.message == WM_QUIT {
                shared.finish.store(true, Ordering::Release);
                break;
            }
            // SAFETY: The message came from this thread's queue and is retained
            // for both synchronous calls; no hook-context borrow crosses them.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    hooks.uninstall(&shared);
    shared.failure().map_or(Ok(()), Err)
}

struct ContextGuard;
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CONTEXT.with(|slot| *slot.borrow_mut() = None);
    }
}

#[derive(Default)]
struct HookGuards {
    mouse: Option<HHOOK>,
    keyboard: Option<HHOOK>,
}
impl HookGuards {
    fn uninstall(&mut self, shared: &Shared) {
        for hook in [self.keyboard.take(), self.mouse.take()]
            .into_iter()
            .flatten()
        {
            // SAFETY: Each handle came from this thread's successful hook install
            // and is taken once; context remains alive during removal.
            if let Err(error) = unsafe { UnhookWindowsHookEx(hook) } {
                shared.fail(failure(InputFailureKind::HookUninstall, error.code().0));
            }
        }
    }
}
impl Drop for HookGuards {
    fn drop(&mut self) {
        for hook in [self.keyboard.take(), self.mouse.take()]
            .into_iter()
            .flatten()
        {
            // SAFETY: Remaining installed hooks are owned here and taken once;
            // this fallback executes before the thread-local context is cleared.
            let _ = unsafe { UnhookWindowsHookEx(hook) };
        }
    }
}

fn held_gesture() -> bool {
    [
        VK_LBUTTON,
        VK_RBUTTON,
        VK_MBUTTON,
        VK_XBUTTON1,
        VK_XBUTTON2,
        VK_ESCAPE,
    ]
    .into_iter()
    // SAFETY: These are documented virtual-key constants; the query retains no
    // pointers and does not generate or consume any input.
    .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0)
}
const fn failure(kind: InputFailureKind, code: i32) -> InputFailure {
    InputFailure { kind, code }
}

/// # Safety
/// USER32 must invoke this with the WH_MOUSE_LL callback contract. For a
/// nonnegative code, lparam borrows a live MSLLHOOKSTRUCT for this call only.
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        // SAFETY: Forward the native hook parameters unchanged without decoding
        // the payload when the hook contract requires immediate propagation.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // SAFETY: This nonnegative callback code guarantees the mouse payload's
    // layout/lifetime. The panic boundary prevents unwinding across system ABI.
    std::panic::catch_unwind(|| unsafe { mouse_hook_inner(code, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Called only from WH_MOUSE_LL with nonnegative code and a valid, aligned
/// MSLLHOOKSTRUCT that USER32 retains until this synchronous callback returns.
unsafe fn mouse_hook_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: The callback contract establishes a live, aligned mouse record;
    // only scalar values are copied into the bounded queue, never this reference.
    let input = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let point = ScreenPointPx {
        x: input.pt.x,
        y: input.pt.y,
    };
    let message = wparam.0 as u32;
    let swallow = CONTEXT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(context) = slot.as_mut() else {
            return false;
        };
        if context.shared.finish.load(Ordering::Acquire) {
            context.protocol.finish();
        }
        if message == WM_MOUSEMOVE {
            if matches!(
                context.protocol.phase(),
                Phase::Capturing | Phase::AwaitDecision
            ) {
                context.shared.moved(point);
            }
            return false;
        }
        if message == WM_MOUSEWHEEL || message == WM_MOUSEHWHEEL {
            let swallow = context.protocol.consumes_wheel();
            if message == WM_MOUSEWHEEL && context.protocol.produces_wheel() {
                let delta = (input.mouseData >> 16) as u16 as i16;
                context.shared.emit(
                    &context.sender,
                    InputEvent::Wheel {
                        session: context.shared.session,
                        point,
                        delta,
                    },
                );
            }
            return swallow;
        }
        let button = match message {
            WM_LBUTTONDOWN | WM_LBUTTONUP => Some((Button::Left, message == WM_LBUTTONDOWN)),
            WM_RBUTTONDOWN | WM_RBUTTONUP => Some((Button::Right, message == WM_RBUTTONDOWN)),
            WM_MBUTTONDOWN | WM_MBUTTONUP => Some((Button::Middle, message == WM_MBUTTONDOWN)),
            WM_XBUTTONDOWN | WM_XBUTTONUP => match (input.mouseData >> 16) as u16 {
                1 => Some((Button::X1, message == WM_XBUTTONDOWN)),
                2 => Some((Button::X2, message == WM_XBUTTONDOWN)),
                _ => None,
            },
            _ => None,
        };
        let Some((button, down)) = button else {
            return false;
        };
        let decision = context.protocol.button(button, down);
        match decision.signal {
            Some(Signal::Candidate) => context.shared.emit(
                &context.sender,
                InputEvent::Candidate {
                    session: context.shared.session,
                    point,
                },
            ),
            Some(Signal::Cancel) => context.shared.emit(
                &context.sender,
                InputEvent::Cancel {
                    session: context.shared.session,
                },
            ),
            None => {}
        }
        decision.swallow
    });
    if swallow {
        LRESULT(1)
    } else {
        // SAFETY: Propagate the original native callback parameters after all
        // thread-local borrows end; the payload remains valid until return.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

/// # Safety
/// USER32 must invoke this with the WH_KEYBOARD_LL callback contract. A
/// nonnegative code supplies a live KBDLLHOOKSTRUCT for the callback duration.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        // SAFETY: The hook contract requires propagation of unchanged parameters
        // without dereferencing the payload for a negative code.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // SAFETY: USER32 supplies the valid keyboard payload for this code; catch
    // any panic here so it cannot unwind through the Windows callback ABI.
    std::panic::catch_unwind(|| unsafe { keyboard_hook_inner(code, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Called only for a nonnegative WH_KEYBOARD_LL callback; lparam points to an
/// aligned KBDLLHOOKSTRUCT that remains live until this callback returns.
unsafe fn keyboard_hook_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: The callback establishes this record's layout and lifetime. The
    // reference is never queued; only the Escape press/release decision escapes.
    let input = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // No key text, modifier state, or other keyboard input is recorded.
    if input.vkCode != u32::from(VK_ESCAPE.0) {
        // SAFETY: Unhandled keys retain the original native hook parameters.
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let down = match wparam.0 as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        // SAFETY: Unrecognized notifications are forwarded unchanged while the
        // original native payload is still live.
        _ => return unsafe { CallNextHookEx(None, code, wparam, lparam) },
    };
    let swallow = CONTEXT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(context) = slot.as_mut() else {
            return false;
        };
        if context.shared.finish.load(Ordering::Acquire) {
            context.protocol.finish();
        }
        let decision = context.protocol.button(Button::Escape, down);
        if decision.signal == Some(Signal::Cancel) {
            context.shared.emit(
                &context.sender,
                InputEvent::Cancel {
                    session: context.shared.session,
                },
            );
        }
        decision.swallow
    });
    if swallow {
        LRESULT(1)
    } else {
        // SAFETY: Forward unchanged parameters only after releasing thread-local
        // borrows; USER32 retains the keyboard payload for the entire callback.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}
