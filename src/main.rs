#![windows_subsystem = "windows"]

mod launch;
mod log;
mod proto;
mod resident;
mod shelf;

fn main() {
    log::now_ms(); // start the clock
    let command = launch::parse_args(std::env::args().skip(1));
    std::process::exit(launch::run(command));
}
