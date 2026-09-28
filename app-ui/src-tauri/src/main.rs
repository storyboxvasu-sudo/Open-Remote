#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("OpenRemote crashed with panic: {:?}", info);
        eprintln!("{}", msg);
        let _ = std::fs::write("open-remote-crash.log", &msg);
    }));

    open_remote_gui_lib::run();
}
