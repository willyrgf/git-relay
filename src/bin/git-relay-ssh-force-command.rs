use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use git_relay::git::SystemGitExecutor;
use git_relay::platform::RealPlatformProbe;
use git_relay::ssh_wrapper::resolve_and_authorize_ssh_command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Parser)]
#[command(name = "git-relay-ssh-force-command")]
#[command(about = "Resolve and execute the Git Relay OpenSSH forced command")]
struct Cli {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    check_only: bool,
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
    let original_command =
        std::env::var("SSH_ORIGINAL_COMMAND").map_err(|_| "SSH_ORIGINAL_COMMAND is not set")?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let resolved =
        resolve_and_authorize_ssh_command(&cli.config, &original_command, &git, &platform)?;

    if cli.check_only {
        println!("{}", serde_json::to_string_pretty(&resolved)?);
        return Ok(ExitCode::SUCCESS);
    }

    #[cfg(unix)]
    {
        let error = std::process::Command::new(&resolved.service)
            .arg(&resolved.repo_path)
            .exec();
        Err(Box::new(error))
    }

    #[cfg(not(unix))]
    {
        let _ = resolved;
        Err("git-relay-ssh-force-command is supported only on Unix platforms".into())
    }
}
