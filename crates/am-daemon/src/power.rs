//! Native power-lifecycle notifications for the daemon.
//!
//! The extension host does not receive a reliable sleep callback. On macOS we
//! subscribe directly to IOKit's root power domain so the daemon can start a
//! cloud handoff before the machine sleeps. The callback acknowledges the
//! power transition within the OS deadline even if a provider is unavailable;
//! the core then leaves the task in its normal pause/resume state.

use am_core::AppCore;
use tokio::task::JoinHandle;

/// Handle for the native power monitor. Shutdown is explicit because the
/// macOS notification loop is a blocking CoreFoundation run loop.
pub struct PowerMonitor {
    #[cfg(target_os = "macos")]
    stop: std::sync::Arc<macos::StopState>,
    #[cfg(target_os = "windows")]
    stop: std::sync::Arc<windows::StopState>,
    join: Option<JoinHandle<()>>,
}

impl PowerMonitor {
    pub async fn shutdown(mut self) {
        #[cfg(target_os = "macos")]
        self.stop.request_stop();
        #[cfg(target_os = "windows")]
        self.stop.request_stop();
        #[cfg(not(target_os = "macos"))]
        if let Some(join) = self.join.take() {
            join.abort();
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

/// Start the platform power monitor. It is intentionally independent of the
/// TCP server so it remains active while the workbench view is hidden.
pub fn spawn(core: AppCore) -> PowerMonitor {
    #[cfg(target_os = "macos")]
    {
        macos::spawn(core)
    }

    #[cfg(target_os = "linux")]
    {
        linux::spawn(core)
    }

    #[cfg(target_os = "windows")]
    {
        windows::spawn(core)
    }

    #[cfg(all(
        not(target_os = "macos"),
        not(target_os = "linux"),
        not(target_os = "windows")
    ))]
    {
        let _ = core;
        let join = tokio::spawn(async {
            tracing::warn!(
                "native sleep notifications are not available on this build; shutdown handoff remains active"
            );
            std::future::pending::<()>().await;
        });
        PowerMonitor { join: Some(join) }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    pub(super) fn spawn(core: AppCore) -> PowerMonitor {
        let join = tokio::spawn(async move {
            let child = Command::new("dbus-monitor")
                .args([
                    "--system",
                    "type='signal',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'",
                    "type='signal',interface='org.freedesktop.login1.Manager',member='PrepareForShutdown'",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn();
            let Ok(mut child) = child else {
                tracing::warn!("dbus-monitor is unavailable; Linux sleep handoff is disabled");
                return;
            };
            let Some(stdout) = child.stdout.take() else {
                tracing::warn!("dbus-monitor did not provide an output stream");
                return;
            };
            tracing::info!("Linux logind power notifications enabled");
            let mut lines = BufReader::new(stdout).lines();
            let mut pending = None;
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("member=PrepareForSleep") {
                    pending = Some(am_core::PowerEvent::SleepImminent);
                } else if line.contains("member=PrepareForShutdown") {
                    pending = Some(am_core::PowerEvent::ShutdownImminent);
                } else if pending.is_some() && line.trim() == "boolean true" {
                    if let Some(event) = pending.take() {
                        core.handle_power_event(event).await;
                    }
                } else if line.trim().starts_with("boolean ") {
                    pending = None;
                }
            }
            let _ = child.kill().await;
        });
        PowerMonitor { join: Some(join) }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostThreadMessageW, RegisterClassW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_MESSAGE,
        MSG, PBT_APMSUSPEND, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT,
        WNDCLASSW,
    };

    struct PowerRequest {
        event: am_core::PowerEvent,
        response: SyncSender<()>,
    }

    pub(super) struct StopState {
        stopped: AtomicBool,
        thread_id: AtomicU32,
    }

    impl StopState {
        fn new() -> Self {
            Self {
                stopped: AtomicBool::new(false),
                thread_id: AtomicU32::new(0),
            }
        }

        pub(super) fn request_stop(&self) {
            self.stopped.store(true, Ordering::Release);
            let thread_id = self.thread_id.load(Ordering::Acquire);
            if thread_id != 0 {
                unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
            }
        }
    }

    static REQUESTS: OnceLock<Mutex<Option<SyncSender<PowerRequest>>>> = OnceLock::new();

    pub(super) fn spawn(core: AppCore) -> PowerMonitor {
        let runtime = tokio::runtime::Handle::current();
        let stop = std::sync::Arc::new(StopState::new());
        let thread_stop = stop.clone();
        let join = tokio::task::spawn_blocking(move || run(core, runtime, thread_stop));
        PowerMonitor {
            stop,
            join: Some(join),
        }
    }

    fn run(core: AppCore, runtime: tokio::runtime::Handle, stop: std::sync::Arc<StopState>) {
        let (requests_tx, requests_rx) = sync_channel::<PowerRequest>(4);
        let requests = REQUESTS.get_or_init(|| Mutex::new(None));
        *requests.lock().expect("power request mutex") = Some(requests_tx);

        let worker = std::thread::spawn(move || {
            while let Ok(request) = requests_rx.recv() {
                runtime.block_on(core.handle_power_event(request.event));
                let _ = request.response.send(());
            }
        });

        let thread_id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        stop.thread_id.store(thread_id, Ordering::Release);
        if stop.stopped.load(Ordering::Acquire) {
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
        }

        let class_name: Vec<u16> = "PerpetualPowerMonitor\0".encode_utf16().collect();
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        unsafe { RegisterClassW(&class) };
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            tracing::warn!("could not create Windows power notification window");
        } else {
            tracing::info!("Windows power notifications enabled");
        }

        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        if !window.is_null() {
            unsafe { DestroyWindow(window) };
        }
        *requests.lock().expect("power request mutex") = None;
        let _ = worker.join();
    }

    unsafe extern "system" fn window_proc(
        _window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let event = if message == WM_POWERBROADCAST && wparam as u32 == PBT_APMSUSPEND {
            Some(am_core::PowerEvent::SleepImminent)
        } else if message == WM_QUERYENDSESSION || (message == WM_ENDSESSION && wparam != 0) {
            Some(am_core::PowerEvent::ShutdownImminent)
        } else {
            None
        };
        if let Some(event) = event {
            if let Some(sender) = REQUESTS
                .get()
                .and_then(|requests| requests.lock().ok()?.as_ref().cloned())
            {
                let (response_tx, response_rx) = sync_channel(0);
                let _ = sender.send(PowerRequest {
                    event,
                    response: response_tx,
                });
                let _ = response_rx.recv_timeout(Duration::from_secs(25));
            }
            return 1;
        }
        DefWindowProcW(_window, message, wparam, lparam)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type IoObject = u32;
    type IoConnect = u32;
    type NotificationPort = *mut c_void;
    type CfRunLoop = *const c_void;
    type CfRunLoopSource = *const c_void;
    type CfString = *const c_void;

    const IO_OBJECT_NULL: IoObject = 0;
    // Values are from IOKit/IOMessage.h's iokit_common_msg(0x270/0x280).
    const IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
    const IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;
    const IO_MESSAGE_SYSTEM_WILL_POWER_OFF: u32 = 0xE000_0250;
    const IO_MESSAGE_SYSTEM_WILL_RESTART: u32 = 0xE000_0310;
    const IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xE000_0300;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            port: *mut NotificationPort,
            callback: extern "C" fn(*mut c_void, IoObject, u32, *mut c_void),
            notifier: *mut IoObject,
        ) -> IoConnect;
        fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
        fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> i32;
        fn IONotificationPortGetRunLoopSource(port: NotificationPort) -> CfRunLoopSource;
        fn IONotificationPortDestroy(port: NotificationPort);
        fn IOServiceClose(connect: IoConnect) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> CfRunLoop;
        fn CFRunLoopAddSource(run_loop: CfRunLoop, source: CfRunLoopSource, mode: CfString);
        fn CFRunLoopStop(run_loop: CfRunLoop);
        fn CFRunLoopRun();
    }

    struct PowerRequest {
        event: am_core::PowerEvent,
        response: SyncSender<()>,
    }

    struct Refcon {
        kernel_port: IoConnect,
        requests: SyncSender<PowerRequest>,
    }

    pub(super) struct StopState {
        stopped: AtomicBool,
        run_loop: Mutex<usize>,
    }

    impl StopState {
        fn new() -> Self {
            Self {
                stopped: AtomicBool::new(false),
                run_loop: Mutex::new(0),
            }
        }

        pub(super) fn request_stop(&self) {
            self.stopped.store(true, Ordering::Release);
            let run_loop = *self.run_loop.lock().expect("power run-loop mutex");
            if run_loop != 0 {
                unsafe { CFRunLoopStop(run_loop as CfRunLoop) };
            }
        }
    }

    pub(super) fn spawn(core: AppCore) -> PowerMonitor {
        let runtime = tokio::runtime::Handle::current();
        let stop = Arc::new(StopState::new());
        let thread_stop = stop.clone();
        let join = tokio::task::spawn_blocking(move || run(core, runtime, thread_stop));
        PowerMonitor {
            stop,
            join: Some(join),
        }
    }

    fn run(core: AppCore, runtime: tokio::runtime::Handle, stop: Arc<StopState>) {
        let (requests_tx, requests_rx) = sync_channel::<PowerRequest>(4);
        let mut port: NotificationPort = ptr::null_mut();
        let mut notifier = IO_OBJECT_NULL;
        let refcon = Box::new(Refcon {
            kernel_port: IO_OBJECT_NULL,
            requests: requests_tx,
        });
        let refcon = Box::into_raw(refcon);
        let kernel_port = unsafe {
            IORegisterForSystemPower(refcon.cast(), &mut port, power_callback, &mut notifier)
        };
        if kernel_port == IO_OBJECT_NULL || port.is_null() {
            tracing::warn!("could not register for macOS power notifications");
            // SAFETY: IOKit did not retain the refcon when registration failed.
            unsafe { drop(Box::from_raw(refcon)) };
            return;
        }
        // The callback needs the kernel port to acknowledge each transition.
        // SAFETY: IOKit has registered this refcon and it remains allocated
        // until process shutdown.
        unsafe { (*refcon).kernel_port = kernel_port };
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        *stop.run_loop.lock().expect("power run-loop mutex") = run_loop as usize;
        let source = unsafe { IONotificationPortGetRunLoopSource(port) };
        // kCFRunLoopDefaultMode is the null-terminated CFString constant
        // exported by CoreFoundation. Passing the symbol through an extern
        // declaration keeps this module free of a CoreFoundation crate.
        let mode = unsafe { kCFRunLoopDefaultMode };
        unsafe { CFRunLoopAddSource(run_loop, source, mode) };
        tracing::info!("macOS power notifications enabled");

        if stop.stopped.load(Ordering::Acquire) {
            unsafe { CFRunLoopStop(run_loop) };
        }

        // The notification callback blocks only until this worker finishes the
        // bounded handoff, then acknowledges the kernel transition. The
        // IOKit callback itself is delivered by this thread's CFRunLoop.
        let worker = std::thread::spawn(move || {
            while let Ok(request) = requests_rx.recv() {
                runtime.block_on(core.handle_power_event(request.event));
                let _ = request.response.send(());
            }
        });
        unsafe { CFRunLoopRun() };
        // Releasing the refcon closes the request sender so the worker exits
        // when the daemon is shutting down.
        unsafe { drop(Box::from_raw(refcon)) };
        let _ = worker.join();

        unsafe {
            IODeregisterForSystemPower(&mut notifier);
            IONotificationPortDestroy(port);
            IOServiceClose(kernel_port);
        }
    }

    extern "C" fn power_callback(
        refcon: *mut c_void,
        _service: IoObject,
        message_type: u32,
        message_argument: *mut c_void,
    ) {
        if refcon.is_null() {
            return;
        }
        // SAFETY: IOKit calls us with the Refcon allocated in `run`, which is
        // kept alive for the monitor lifetime.
        let state = unsafe { &*(refcon.cast::<Refcon>()) };
        if message_type == IO_MESSAGE_CAN_SYSTEM_SLEEP {
            let notification_id = message_argument as isize;
            let (response_tx, response_rx) = sync_channel(0);
            let request = PowerRequest {
                event: am_core::PowerEvent::SleepImminent,
                response: response_tx,
            };
            if state.requests.send(request).is_ok() {
                // macOS gives clients at most 30 seconds to acknowledge this
                // notification. Let the machine proceed rather than freezing
                // shutdown forever if a provider/network is unavailable.
                let _ = response_rx.recv_timeout(Duration::from_secs(25));
            }
            unsafe { IOAllowPowerChange(state.kernel_port, notification_id) };
        } else if message_type == IO_MESSAGE_SYSTEM_WILL_SLEEP {
            let notification_id = message_argument as isize;
            unsafe { IOAllowPowerChange(state.kernel_port, notification_id) };
        } else if message_type == IO_MESSAGE_SYSTEM_WILL_POWER_OFF
            || message_type == IO_MESSAGE_SYSTEM_WILL_RESTART
        {
            let (response_tx, response_rx) = sync_channel(0);
            let _ = state.requests.send(PowerRequest {
                event: am_core::PowerEvent::ShutdownImminent,
                response: response_tx,
            });
            let _ = response_rx.recv_timeout(Duration::from_secs(25));
        } else if message_type == IO_MESSAGE_SYSTEM_HAS_POWERED_ON {
            tracing::debug!("macOS system wake notification received");
        }
    }

    extern "C" {
        static kCFRunLoopDefaultMode: CfString;
    }
}
