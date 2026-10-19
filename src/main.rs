#![windows_subsystem = "windows"]

mod launch;
mod log;
mod proto;
mod resident;

fn main() {
    log::now_ms(); // start the clock
    let command = launch::parse_args(std::env::args().skip(1));
    std::process::exit(launch::run(command));
}
