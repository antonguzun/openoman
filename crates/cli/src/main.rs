mod app;
mod cli;
mod config;
mod internal;
mod launcher;
mod logging;
mod server;
mod service;

use std::process;

#[tokio::main]
async fn main() {
    logging::init();
    if let Err(err) = app::run().await {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
