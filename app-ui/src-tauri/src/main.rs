#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn get_crash_log_path() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let dir = PathBuf::from(local_app_data).join("open-remote");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("crash.log")
    } else {
        PathBuf::from("open-remote-crash.log")
    }
}

fn show_error_dialog(message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

        let title: Vec<u16> = "OpenRemote - Fatal Error\0".encode_utf16().collect();
        let body: Vec<u16> = format!("{}\0", message).encode_utf16().collect();

        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("{}", message);
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let crash_file = get_crash_log_path();
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic payload".to_string()
        };

        let location = if let Some(loc) = info.location() {
            format!("{}:{}:{}", loc.file(), loc.line(), loc.column())
        } else {
            "Unknown location".to_string()
        };

        let backtrace = std::backtrace::Backtrace::capture();
        let log_content = format!(
            "=== OpenRemote Crash Report ===\nTime: {:?}\nLocation: {}\nError: {}\nBacktrace:\n{}\n===============================\n",
            std::time::SystemTime::now(),
            location,
            payload,
            backtrace
        );

        let _ = std::fs::write(&crash_file, &log_content);

        let dialog_msg = format!(
            "OpenRemote encountered an unexpected error and needs to close.\n\n\
            Location: {}\n\
            Details: {}\n\n\
            A detailed crash log was saved to:\n{}",
            location,
            payload,
            crash_file.display()
        );

        show_error_dialog(&dialog_msg);
    }));

    open_remote_gui_lib::run();
}
