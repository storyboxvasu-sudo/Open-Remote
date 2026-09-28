use app_common::{InputEvent, MouseButton};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum InputError {
    #[error("Platform input injection failed: {0}")]
    InjectionFailed(String),
    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

pub struct InputInjector;

impl InputInjector {
    pub fn new() -> Self {
        Self
    }

    #[cfg(windows)]
    pub fn inject(&self, event: &InputEvent) -> Result<(), InputError> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

        unsafe {
            match event {
                InputEvent::MouseMove { x, y } => {

                    let abs_x = ((x * 65535.0).round()) as i32;
                    let abs_y = ((y * 65535.0).round()) as i32;

                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: abs_x,
                                dy: abs_y,
                                mouseData: 0,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::MouseDown { button, x, y } => {
                    let abs_x = ((x * 65535.0).round()) as i32;
                    let abs_y = ((y * 65535.0).round()) as i32;
                    let dw_flags = match button {
                        MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
                        MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
                        MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
                    };
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: abs_x,
                                dy: abs_y,
                                mouseData: 0,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | dw_flags | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::MouseUp { button, x, y } => {
                    let abs_x = ((x * 65535.0).round()) as i32;
                    let abs_y = ((y * 65535.0).round()) as i32;
                    let dw_flags = match button {
                        MouseButton::Left => MOUSEEVENTF_LEFTUP,
                        MouseButton::Right => MOUSEEVENTF_RIGHTUP,
                        MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
                    };
                    let mut input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: abs_x,
                                dy: abs_y,
                                mouseData: 0,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | dw_flags | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
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
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::KeyDown { scancode, .. } => {
                    let mut input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: 0,
                                wScan: *scancode as u16,
                                dwFlags: KEYEVENTF_SCANCODE,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::KeyUp { scancode, .. } => {
                    let mut input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: 0,
                                wScan: *scancode as u16,
                                dwFlags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
                }
                InputEvent::ClipboardSync { .. } => {
                    // Handled at clipboard subsystem layer
                }
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn inject(&self, _event: &InputEvent) -> Result<(), InputError> {
        Err(InputError::UnsupportedPlatform)
    }
}
