mod app;
mod cli;
mod config;
mod internal;
mod server;
mod service;

use std::process;

#[tokio::main]
async fn main() {
    if let Err(err) = app::run().await {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
