#![windows_subsystem = "windows"]

mod hotkey;
mod launch;
mod log;
mod park;
mod proto;
mod resident;
mod settings;
mod settings_win;
mod shelf;
mod startup;
mod thumbs;
mod tray;

fn main() {
    log::now_ms(); // start the clock
    let command = launch::parse_args(std::env::args().skip(1));
    std::process::exit(launch::run(command));
}
