use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use git_relay::hooks::install_hooks;

#[derive(Debug, Parser)]
#[command(name = "git-relay-install-hooks")]
#[command(about = "Install Git Relay hook wrappers into a bare repository")]
struct Cli {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    dispatcher: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let hooks = install_hooks(&cli.repo, &cli.dispatcher)?;
    println!("{}", serde_json::to_string_pretty(&hooks)?);
    Ok(ExitCode::SUCCESS)
}
