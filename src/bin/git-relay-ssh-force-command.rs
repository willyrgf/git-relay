use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use git_relay::git::SystemGitExecutor;
use git_relay::platform::RealPlatformProbe;
use git_relay::ssh_wrapper::resolve_and_authorize_ssh_command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const REQUEST_ID_ENV: &str = "GIT_RELAY_REQUEST_ID";
const PUSH_ID_ENV: &str = "GIT_RELAY_PUSH_ID";

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
        let mut command = std::process::Command::new(&resolved.service);
        command
            .arg(&resolved.repo_path)
            .env(REQUEST_ID_ENV, generate_session_id("request"));
        if resolved.service == "git-receive-pack" {
            command.env(PUSH_ID_ENV, generate_session_id("push"));
        }
        let error = command.exec();
        Err(Box::new(error))
    }

    #[cfg(not(unix))]
    {
        let _ = resolved;
        Err("git-relay-ssh-force-command is supported only on Unix platforms".into())
    }
}

fn generate_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}
