//! Session-scoped low-level input hooks. The hook thread only copies input,
//! maintains gesture ownership, and posts bounded/coalesced notifications.

#[path = "input_protocol.rs"]
mod protocol;

use std::{
    cell::RefCell,
    marker::PhantomData,
    os::windows::io::AsRawHandle,
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
            CloseHandle, E_FAIL, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WAIT_FAILED,
            WAIT_TIMEOUT, WPARAM,
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
            InputFailureKind::GestureAlreadyHeld => "请先松开鼠标按钮和 Esc，再开始取色",
            InputFailureKind::HookInstall => "无法安装本次会话的输入钩子",
            InputFailureKind::EventQueueFull => "输入事件队列已满，本次取色已取消",
            InputFailureKind::ReceiverDisconnected => "输入事件接收端已关闭",
            InputFailureKind::WakeFailed => "无法唤醒取色窗口",
            InputFailureKind::ControlWakeFailed => "无法唤醒输入线程进行清理",
            InputFailureKind::MessageLoop => "输入线程消息等待失败",
            InputFailureKind::DrainTimeout => "等待已按下的按键释放超时，本次取色已取消",
            InputFailureKind::HookUninstall => "输入钩子卸载失败",
            InputFailureKind::ThreadPanicked => "输入线程异常退出",
            InputFailureKind::DpiContext => "输入线程未使用 PerMonitorV2 DPI 上下文",
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
            return Err(Error::new(E_FAIL, "请先松开鼠标按钮和 Esc，再开始取色"));
        }
        let signal = ControlSignal::new()?;
        let shared = Arc::new(Shared {
            session,
            notify: notify.0 as usize,
            control: signal,
            finish: AtomicBool::new(false),
            reject: AtomicBool::new(false),
            failure: AtomicU64::new(0),
            movement: AtomicU64::new(0),
            movement_pending: AtomicBool::new(false),
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
            .map_err(|error| Error::new(E_FAIL, format!("无法创建输入线程：{error}")))?;
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
        // Clear before reading so a racing producer always retains a wakeup.
        if !self.shared.movement_pending.swap(false, Ordering::AcqRel) {
            return None;
        }
        let packed = self.shared.movement.load(Ordering::Acquire);
        Some(ScreenPointPx {
            x: packed as u32 as i32,
            y: (packed >> 32) as u32 as i32,
        })
    }
    pub fn reject_candidate(&self) -> Result<()> {
        self.shared.reject.store(true, Ordering::Release);
        self.shared.signal_control()
    }
    pub fn request_finish(&self) -> Result<()> {
        self.shared.finish.store(true, Ordering::Release);
        self.shared.signal_control()
    }
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Borrow the worker's native waitable thread handle. The caller must not
    /// close it, transfer ownership, or retain it across try_join/this owner's
    /// destruction. Waiting alongside the UI queue requires no polling timer.
    pub fn wait_handle(&self) -> Option<HANDLE> {
        self.worker
            .as_ref()
            .map(|worker| HANDLE(worker.as_raw_handle()))
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

// Kernel event handles are explicitly designed for cross-thread signaling.
// Store the opaque value rather than assigning Send/Sync to any Win32 type.
struct ControlSignal(usize);
impl ControlSignal {
    fn new() -> Result<Self> {
        Ok(Self(
            unsafe { CreateEventW(None, false, false, None)? }.0 as usize,
        ))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }
    fn set(&self) -> Result<()> {
        unsafe { SetEvent(self.handle()) }
    }
}
impl Drop for ControlSignal {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle()) };
    }
}

struct Shared {
    session: SessionId,
    notify: usize,
    control: ControlSignal,
    finish: AtomicBool,
    reject: AtomicBool,
    failure: AtomicU64,
    movement: AtomicU64,
    movement_pending: AtomicBool,
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
        let packed = u64::from(point.x as u32) | (u64::from(point.y as u32) << 32);
        self.movement.store(packed, Ordering::Release);
        if !self.movement_pending.swap(true, Ordering::AcqRel) {
            self.notify();
        }
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
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| failure(InputFailureKind::HookInstall, error.code().0))?;
    hooks.mouse = Some(
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(HINSTANCE(module.0)), 0) }
            .map_err(|error| failure(InputFailureKind::HookInstall, error.code().0))?,
    );
    hooks.keyboard = Some(
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
        let handles = [shared.control.handle()];
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
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                break;
            }
            if message.message == WM_QUIT {
                shared.finish.store(true, Ordering::Release);
                break;
            }
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
    .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0)
}
const fn failure(kind: InputFailureKind, code: i32) -> InputFailure {
    InputFailure { kind, code }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    std::panic::catch_unwind(|| unsafe { mouse_hook_inner(code, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

unsafe fn mouse_hook_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    std::panic::catch_unwind(|| unsafe { keyboard_hook_inner(code, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

unsafe fn keyboard_hook_inner(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let input = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // No key text, modifier state, or other keyboard input is recorded.
    if input.vkCode != u32::from(VK_ESCAPE.0) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let down = match wparam.0 as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
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
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}
