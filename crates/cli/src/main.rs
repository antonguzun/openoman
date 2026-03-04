mod app;
mod cli;
mod config;
mod internal;

use std::process;

fn main() {
    if let Err(err) = app::run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
