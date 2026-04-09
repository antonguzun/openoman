use std::{net::Ipv4Addr, path::PathBuf};

use clap::ArgAction;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "openoman")]
#[command(about = "OpenOMAN MVP CLI")]
pub(crate) struct Cli {
    /// Path to config file.
    #[arg(long, default_value = "config.toml")]
    pub(crate) config: PathBuf,
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    Serve,
    Api,
    Launcher,
    Dev,
    Stack {
        #[command(subcommand)]
        command: StackCommands,
    },
    Submit {
        /// Repository alias (preferred) or raw clone URL/path.
        #[arg(long)]
        repo: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        instruction: String,
        #[arg(long, default_value = "unit")]
        check_profile: String,
        #[arg(long, default_value = "on_validation_success")]
        publish_policy: String,
    },
    Run {
        job_id: String,
    },
    Status {
        job_id: String,
    },
    Logs {
        job_id: String,
    },
    Artifacts {
        job_id: String,
    },
    Result {
        job_id: String,
    },
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum StackCommands {
    Start,
    Stop,
    Status,
}

#[derive(Debug, Subcommand)]
pub(crate) enum InternalCommands {
    FirecrackerNet {
        #[command(subcommand)]
        command: FirecrackerNetCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum FirecrackerNetCommands {
    Setup {
        #[arg(long)]
        tap_name: String,
        #[arg(long)]
        host_ip: Ipv4Addr,
        #[arg(long)]
        prefix_len: u8,
        #[arg(long, action = ArgAction::Set, default_value_t = false)]
        allow_all: bool,
    },
    Teardown {
        #[arg(long)]
        tap_name: String,
    },
}
