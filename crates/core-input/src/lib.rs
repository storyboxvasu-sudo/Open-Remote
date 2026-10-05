use app_common::{InputEvent, MouseButton};
use std::collections::HashSet;
use std::sync::RwLock;
use thiserror::Error;

pub const REMOTE_INPUT_TAG: usize = 0x52454D4F; // "REMO"

#[derive(Error, Debug)]
pub enum InputError {
    #[error("Platform input injection failed: {0}")]
    InjectionFailed(String),
    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

pub struct InputInjector {
    /// Active target monitor bounds: (x, y, width, height)
    active_monitor: RwLock<(i32, i32, u32, u32)>,
    held_keys: RwLock<HashSet<u32>>,
    held_mouse_buttons: RwLock<HashSet<MouseButton>>,
}

impl InputInjector {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
            };
            let w = unsafe { GetSystemMetrics(SM_CXSCREEN) } as u32;
            let h = unsafe { GetSystemMetrics(SM_CYSCREEN) } as u32;
            let initial_w = if w == 0 { 1920 } else { w };
            let initial_h = if h == 0 { 1080 } else { h };
            Self {
                active_monitor: RwLock::new((0, 0, initial_w, initial_h)),
                held_keys: RwLock::new(HashSet::new()),
                held_mouse_buttons: RwLock::new(HashSet::new()),
            }
        }
        #[cfg(target_os = "macos")]
        {
            let bounds = unsafe {
                let main_id = macos_input::CGMainDisplayID();
                macos_input::CGDisplayBounds(main_id)
            };
            let w = if bounds.size.width > 0.0 { bounds.size.width as u32 } else { 1920 };
            let h = if bounds.size.height > 0.0 { bounds.size.height as u32 } else { 1080 };
            Self {
                active_monitor: RwLock::new((bounds.origin.x as i32, bounds.origin.y as i32, w, h)),
                held_keys: RwLock::new(HashSet::new()),
                held_mouse_buttons: RwLock::new(HashSet::new()),
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Self {
                active_monitor: RwLock::new((0, 0, 1920, 1080)),
                held_keys: RwLock::new(HashSet::new()),
                held_mouse_buttons: RwLock::new(HashSet::new()),
            }
        }
    }

    pub fn set_active_monitor_bounds(&self, x: i32, y: i32, width: u32, height: u32) {
        if let Ok(mut lock) = self.active_monitor.write() {
            *lock = (x, y, width, height);
        }
    }

    pub fn release_all(&self) {
        if let Ok(mut buttons) = self.held_mouse_buttons.write() {
            let drained: Vec<MouseButton> = buttons.drain().collect();
            for button in drained {
                let _ = self.inject(&InputEvent::MouseUp {
                    button,
                    x: 0.5,
                    y: 0.5,
                });
            }
        }
        if let Ok(mut keys) = self.held_keys.write() {
            let drained: Vec<u32> = keys.drain().collect();
            for scancode in drained {
                let _ = self.inject(&InputEvent::KeyUp {
                    scancode,
                    key: String::new(),
                });
            }
        }
    }

    #[cfg(target_os = "windows")]
    pub fn inject(&self, event: &InputEvent) -> Result<(), InputError> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;

        let (mon_x, mon_y, mon_w, mon_h) = self
            .active_monitor
            .read()
            .map(|g| *g)
            .unwrap_or((0, 0, 1920, 1080));

        unsafe {
            match event {
                InputEvent::MouseMove { x, y } => {
                    let target_x = mon_x + ((x.clamp(0.0, 1.0) * mon_w as f64).round() as i32);
                    let target_y = mon_y + ((y.clamp(0.0, 1.0) * mon_h as f64).round() as i32);
                    SetCursorPos(target_x, target_y);
                }
                InputEvent::MouseDown { button, x, y } => {
                    let target_x = mon_x + ((x.clamp(0.0, 1.0) * mon_w as f64).round() as i32);
                    let target_y = mon_y + ((y.clamp(0.0, 1.0) * mon_h as f64).round() as i32);
                    SetCursorPos(target_x, target_y);

                    if let Ok(mut set) = self.held_mouse_buttons.write() {
                        set.insert(*button);
                    }

                    let dw_flags = match button {
                        MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
                        MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
                        MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
                    };
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: 0,
                                dwFlags: dw_flags,
                                time: 0,
                                dwExtraInfo: REMOTE_INPUT_TAG,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::MouseUp { button, x, y } => {
                    let target_x = mon_x + ((x.clamp(0.0, 1.0) * mon_w as f64).round() as i32);
                    let target_y = mon_y + ((y.clamp(0.0, 1.0) * mon_h as f64).round() as i32);
                    SetCursorPos(target_x, target_y);

                    if let Ok(mut set) = self.held_mouse_buttons.write() {
                        set.remove(button);
                    }

                    let dw_flags = match button {
                        MouseButton::Left => MOUSEEVENTF_LEFTUP,
                        MouseButton::Right => MOUSEEVENTF_RIGHTUP,
                        MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
                    };
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: 0,
                                dwFlags: dw_flags,
                                time: 0,
                                dwExtraInfo: REMOTE_INPUT_TAG,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::MouseWheel { delta_y, .. } => {
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: *delta_y as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                time: 0,
                                dwExtraInfo: REMOTE_INPUT_TAG,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::KeyDown { scancode, .. } => {
                    if let Ok(mut set) = self.held_keys.write() {
                        set.insert(*scancode);
                    }
                    let mut input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: *scancode as u16,
                                wScan: 0,
                                dwFlags: 0,
                                time: 0,
                                dwExtraInfo: REMOTE_INPUT_TAG,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::KeyUp { scancode, .. } => {
                    if let Ok(mut set) = self.held_keys.write() {
                        set.remove(scancode);
                    }
                    let mut input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: *scancode as u16,
                                wScan: 0,
                                dwFlags: KEYEVENTF_KEYUP,
                                time: 0,
                                dwExtraInfo: REMOTE_INPUT_TAG,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::SwitchMonitor { .. } => {
                    // Handled at host routing level
                }
                InputEvent::ClipboardSync { .. } => {
                    // Handled at clipboard subsystem layer
                }
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn inject(&self, event: &InputEvent) -> Result<(), InputError> {
        use macos_input::*;

        let (mon_x, mon_y, mon_w, mon_h) = self
            .active_monitor
            .read()
            .map(|g| *g)
            .unwrap_or((0, 0, 1920, 1080));

        unsafe {
            match event {
                InputEvent::MouseMove { x, y } => {
                    let target_x = mon_x as f64 + (x.clamp(0.0, 1.0) * mon_w as f64);
                    let target_y = mon_y as f64 + (y.clamp(0.0, 1.0) * mon_h as f64);
                    let pos = CGPoint { x: target_x, y: target_y };
                    CGWarpMouseCursorPosition(pos);
                    let ev = CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        kCGEventMouseMoved,
                        pos,
                        kCGMouseButtonLeft,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::MouseDown { button, x, y } => {
                    let target_x = mon_x as f64 + (x.clamp(0.0, 1.0) * mon_w as f64);
                    let target_y = mon_y as f64 + (y.clamp(0.0, 1.0) * mon_h as f64);
                    let pos = CGPoint { x: target_x, y: target_y };
                    CGWarpMouseCursorPosition(pos);

                    if let Ok(mut set) = self.held_mouse_buttons.write() {
                        set.insert(*button);
                    }

                    let (ev_type, btn) = match button {
                        MouseButton::Left => (kCGEventLeftMouseDown, kCGMouseButtonLeft),
                        MouseButton::Right => (kCGEventRightMouseDown, kCGMouseButtonRight),
                        MouseButton::Middle => (kCGEventOtherMouseDown, kCGMouseButtonCenter),
                    };
                    let ev = CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        ev_type,
                        pos,
                        btn,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::MouseUp { button, x, y } => {
                    let target_x = mon_x as f64 + (x.clamp(0.0, 1.0) * mon_w as f64);
                    let target_y = mon_y as f64 + (y.clamp(0.0, 1.0) * mon_h as f64);
                    let pos = CGPoint { x: target_x, y: target_y };
                    CGWarpMouseCursorPosition(pos);

                    if let Ok(mut set) = self.held_mouse_buttons.write() {
                        set.remove(button);
                    }

                    let (ev_type, btn) = match button {
                        MouseButton::Left => (kCGEventLeftMouseUp, kCGMouseButtonLeft),
                        MouseButton::Right => (kCGEventRightMouseUp, kCGMouseButtonRight),
                        MouseButton::Middle => (kCGEventOtherMouseUp, kCGMouseButtonCenter),
                    };
                    let ev = CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        ev_type,
                        pos,
                        btn,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::MouseWheel { delta_y, .. } => {
                    let ev = CGEventCreateScrollWheelEvent(
                        std::ptr::null_mut(),
                        0,
                        1,
                        *delta_y * 10,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::KeyDown { scancode, key } => {
                    if let Ok(mut set) = self.held_keys.write() {
                        set.insert(*scancode);
                    }
                    let key_code = map_key_to_macos_keycode(*scancode, key);
                    let ev = CGEventCreateKeyboardEvent(
                        std::ptr::null_mut(),
                        key_code,
                        true,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::KeyUp { scancode, key } => {
                    if let Ok(mut set) = self.held_keys.write() {
                        set.remove(scancode);
                    }
                    let key_code = map_key_to_macos_keycode(*scancode, key);
                    let ev = CGEventCreateKeyboardEvent(
                        std::ptr::null_mut(),
                        key_code,
                        false,
                    );
                    if !ev.is_null() {
                        CGEventPost(kCGHIDEventTap, ev);
                        CFRelease(ev as CFTypeRef);
                    }
                }
                InputEvent::SwitchMonitor { .. } => {}
                InputEvent::ClipboardSync { .. } => {}
            }
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub fn inject(&self, _event: &InputEvent) -> Result<(), InputError> {
        Err(InputError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
pub mod macos_input {

    pub type CGDirectDisplayID = u32;
    pub type CGError = i32;
    pub type CGFloat = f64;
    pub type CGKeyCode = u16;
    pub type CGEventType = u32;
    pub type CGMouseButton = u32;
    pub type CGEventTapLocation = u32;
    pub type CGScrollEventUnit = u32;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGPoint {
        pub x: CGFloat,
        pub y: CGFloat,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGSize {
        pub width: CGFloat,
        pub height: CGFloat,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    pub enum CGEvent {}
    pub type CGEventRef = *mut CGEvent;

    pub enum CGEventSource {}
    pub type CGEventSourceRef = *mut CGEventSource;

    pub type CFTypeRef = *const std::ffi::c_void;

    pub const kCGHIDEventTap: CGEventTapLocation = 0;

    pub const kCGEventLeftMouseDown: CGEventType = 1;
    pub const kCGEventLeftMouseUp: CGEventType = 2;
    pub const kCGEventRightMouseDown: CGEventType = 3;
    pub const kCGEventRightMouseUp: CGEventType = 4;
    pub const kCGEventMouseMoved: CGEventType = 5;
    pub const kCGEventKeyDown: CGEventType = 10;
    pub const kCGEventKeyUp: CGEventType = 11;
    pub const kCGEventScrollWheel: CGEventType = 22;
    pub const kCGEventOtherMouseDown: CGEventType = 25;
    pub const kCGEventOtherMouseUp: CGEventType = 26;

    pub const kCGMouseButtonLeft: CGMouseButton = 0;
    pub const kCGMouseButtonRight: CGMouseButton = 1;
    pub const kCGMouseButtonCenter: CGMouseButton = 2;

    #[link(name = "CoreGraphics", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn CGMainDisplayID() -> CGDirectDisplayID;
        pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        pub fn CGEventCreateMouseEvent(
            source: CGEventSourceRef,
            mouseType: CGEventType,
            mouseCursorPosition: CGPoint,
            mouseButton: CGMouseButton,
        ) -> CGEventRef;

        pub fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtualKey: CGKeyCode,
            keyDown: bool,
        ) -> CGEventRef;

        pub fn CGEventCreateScrollWheelEvent(
            source: CGEventSourceRef,
            units: CGScrollEventUnit,
            wheelCount: u32,
            wheel1: i32,
        ) -> CGEventRef;

        pub fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
        pub fn CGWarpMouseCursorPosition(newCursorPosition: CGPoint) -> CGError;
        pub fn CFRelease(cf: CFTypeRef);
    }

    pub fn map_key_to_macos_keycode(scancode: u32, key: &str) -> u16 {
        match key {
            "a" | "A" => 0,
            "s" | "S" => 1,
            "d" | "D" => 2,
            "f" | "F" => 3,
            "h" | "H" => 4,
            "g" | "G" => 5,
            "z" | "Z" => 6,
            "x" | "X" => 7,
            "c" | "C" => 8,
            "v" | "V" => 9,
            "b" | "B" => 11,
            "q" | "Q" => 12,
            "w" | "W" => 13,
            "e" | "E" => 14,
            "r" | "R" => 15,
            "y" | "Y" => 16,
            "t" | "T" => 17,
            "1" | "!" => 18,
            "2" | "@" => 19,
            "3" | "#" => 20,
            "4" | "$" => 21,
            "6" | "^" => 22,
            "5" | "%" => 23,
            "=" | "+" => 24,
            "9" | "(" => 25,
            "7" | "&" => 26,
            "-" | "_" => 27,
            "8" | "*" => 28,
            "0" | ")" => 29,
            "]" | "}" => 30,
            "o" | "O" => 31,
            "u" | "U" => 32,
            "[" | "{" => 33,
            "i" | "I" => 34,
            "p" | "P" => 35,
            "Enter" | "\n" | "\r" => 36,
            "l" | "L" => 37,
            "j" | "J" => 38,
            "'" | "\"" => 39,
            "k" | "K" => 40,
            ";" | ":" => 41,
            "\\" | "|" => 42,
            "," | "<" => 43,
            "/" | "?" => 44,
            "n" | "N" => 45,
            "m" | "M" => 46,
            "." | ">" => 47,
            "Tab" => 48,
            " " | "Space" => 49,
            "`" | "~" => 50,
            "Backspace" => 51,
            "Escape" => 53,
            "Meta" | "Command" => 55,
            "Shift" => 56,
            "CapsLock" => 57,
            "Alt" | "Option" => 58,
            "Control" => 59,
            "ArrowLeft" => 123,
            "ArrowRight" => 124,
            "ArrowDown" => 125,
            "ArrowUp" => 126,
            _ => {
                if scancode <= 127 {
                    scancode as u16
                } else {
                    0
                }
            }
        }
    }
}

pub use host_input_monitor::{ensure_host_input_monitor, is_host_input_active};

#[cfg(target_os = "windows")]
mod host_input_monitor {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static LAST_PHYSICAL_INPUT_MS: AtomicU64 = AtomicU64::new(0);
    static MONITOR_RUNNING: AtomicBool = AtomicBool::new(false);

    fn current_time_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    unsafe extern "system" fn low_level_mouse_proc(
        n_code: i32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        if n_code >= 0 {
            let mouse_info = *(l_param as *const MSLLHOOKSTRUCT);
            // LLMHF_INJECTED = 0x00000001
            // If LLMHF_INJECTED is not set and dwExtraInfo is not our remote injection tag:
            if (mouse_info.flags & LLMHF_INJECTED) == 0 && mouse_info.dwExtraInfo != crate::REMOTE_INPUT_TAG {
                LAST_PHYSICAL_INPUT_MS.store(current_time_ms(), Ordering::Relaxed);
            }
        }
        CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param)
    }

    unsafe extern "system" fn low_level_keyboard_proc(
        n_code: i32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        if n_code >= 0 {
            let kbd_info = *(l_param as *const KBDLLHOOKSTRUCT);
            // LLKHF_INJECTED = 0x00000010
            // If LLKHF_INJECTED is not set and dwExtraInfo is not our remote injection tag:
            if (kbd_info.flags & LLKHF_INJECTED) == 0 && kbd_info.dwExtraInfo != crate::REMOTE_INPUT_TAG {
                LAST_PHYSICAL_INPUT_MS.store(current_time_ms(), Ordering::Relaxed);
            }
        }
        CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param)
    }

    pub fn ensure_host_input_monitor() {
        if MONITOR_RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }

        std::thread::Builder::new()
            .name("host-input-monitor".to_string())
            .spawn(move || unsafe {
                let mouse_hook = SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(low_level_mouse_proc),
                    std::ptr::null_mut(),
                    0,
                );
                let kbd_hook = SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(low_level_keyboard_proc),
                    std::ptr::null_mut(),
                    0,
                );

                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }

                if !mouse_hook.is_null() {
                    UnhookWindowsHookEx(mouse_hook);
                }
                if !kbd_hook.is_null() {
                    UnhookWindowsHookEx(kbd_hook);
                }
                MONITOR_RUNNING.store(false, Ordering::SeqCst);
            })
            .ok();
    }

    pub fn is_host_input_active(cooldown_ms: u64) -> bool {
        let last = LAST_PHYSICAL_INPUT_MS.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = current_time_ms();
        now.saturating_sub(last) < cooldown_ms
    }
}

#[cfg(not(target_os = "windows"))]
mod host_input_monitor {
    pub fn ensure_host_input_monitor() {}
    pub fn is_host_input_active(_cooldown_ms: u64) -> bool {
        false
    }
}

