use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use git_relay::crash::{self, CrashCheckpoint};
use git_relay::git::SystemGitExecutor;
use git_relay::platform::RealPlatformProbe;
use git_relay::ssh_wrapper::{resolve_and_authorize_ssh_command, AuthorizedSshCommand};

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
        if crash::checkpointing_enabled() {
            return execute_proxy(&resolved);
        }
        let mut command = build_command(&resolved);
        let error = command.exec();
        Err(Box::new(error))
    }

    #[cfg(not(unix))]
    {
        let _ = resolved;
        Err("git-relay-ssh-force-command is supported only on Unix platforms".into())
    }
}

#[cfg(unix)]
fn execute_proxy(resolved: &AuthorizedSshCommand) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut command = build_command(resolved);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or("git child stdout was not piped")?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or("git child stderr was not piped")?;

    let stdout_forwarder =
        thread::spawn(move || forward_stream(child_stdout, StreamTarget::Stdout));
    let stderr_forwarder =
        thread::spawn(move || forward_stream(child_stderr, StreamTarget::Stderr));

    let status = child.wait()?;
    if status.success() {
        crash::hit_checkpoint(CrashCheckpoint::AfterReceivePackSuccessBeforeWrapperExit);
    }

    let _ = stdout_forwarder.join();
    let _ = stderr_forwarder.join();
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();

    if status.success() {
        crash::hit_checkpoint(CrashCheckpoint::AfterWrapperFlushesResponse);
    }

    Ok(exit_code_from_status(status))
}

#[cfg(unix)]
fn build_command(resolved: &AuthorizedSshCommand) -> Command {
    let mut command = Command::new(&resolved.service);
    command.arg(&resolved.repo_path);
    if std::env::var_os(REQUEST_ID_ENV).is_none() {
        command.env(REQUEST_ID_ENV, generate_session_id("request"));
    }
    if resolved.service == "git-receive-pack" && std::env::var_os(PUSH_ID_ENV).is_none() {
        command.env(PUSH_ID_ENV, generate_session_id("push"));
    }
    command
}

#[cfg(unix)]
fn exit_code_from_status(status: ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) if code == 0 => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code as u8),
        None => ExitCode::from(1),
    }
}

#[cfg(unix)]
fn forward_stream<R: Read>(mut reader: R, target: StreamTarget) {
    let mut buffer = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        let result = match target {
            StreamTarget::Stdout => {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(&buffer[..read])
                    .and_then(|_| stdout.flush())
            }
            StreamTarget::Stderr => {
                let mut stderr = io::stderr().lock();
                stderr
                    .write_all(&buffer[..read])
                    .and_then(|_| stderr.flush())
            }
        };
        if result.is_err() {
            break;
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum StreamTarget {
    Stdout,
    Stderr,
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
