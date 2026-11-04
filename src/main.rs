#![windows_subsystem = "windows"]

mod clean;
mod cli;
mod monitor;
mod settings;
mod ui;
mod win;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--helper") => clean::helper::serve(&args[1..]),
        Some("--tray") => ui::run(true),
        Some(_) => {
            attach_console();
            cli::run(&args)
        }
        None => ui::run(false),
    };
    std::process::exit(code);
}

/// GUI-subsystem exe: borrow the parent's console for CLI output.
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.ok();
}
