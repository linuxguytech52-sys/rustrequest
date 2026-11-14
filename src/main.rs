mod cli;
mod config;
mod crawler;
mod export;
mod extract;
mod gui;
mod headless;
mod http;
mod js;
mod robots;
mod tui;
mod util;

use clap::Parser;

use crate::cli::Cli;
use crate::config::Config;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = Config::from_cli(cli)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if cfg.gui {
        match gui::run(cfg.clone()) {
            Ok(()) => Ok(()),
            Err(err) => {
                eprintln!("GUI unavailable: {err:#}");
                eprintln!("Falling back to headless mode.");
                runtime.block_on(headless::run(cfg))
            }
        }
    } else if cfg.tui {
        tui::run(&runtime, cfg)
    } else {
        runtime.block_on(headless::run(cfg))
    }
}
