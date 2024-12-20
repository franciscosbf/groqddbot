use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use groqddbot::{bot, config, log};

/// LLM chat bot
#[derive(Parser, Debug)]
struct Args {
    /// Config file
    #[arg(short, long)]
    config: PathBuf,
}

#[tokio::main()]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let conf = config::App::parse(&args.config).context("Failed to parse config")?;

    log::init();

    bot::run(conf).await.context("Unexpected error on bot")
}
