use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use git_relay::config::AppConfig;
use git_relay::deploy::validate_runtime_profile;
use git_relay::git::SystemGitExecutor;
use git_relay::platform::RealPlatformProbe;
use git_relay::reconcile::{process_pending_reconcile_requests, ReconcileRunRecord};
use git_relay::validator::Validator;

#[derive(Debug, Parser)]
#[command(name = "git-relayd")]
#[command(about = "Git Relay daemon entrypoint")]
struct DaemonCli {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    once: bool,
}

#[derive(Debug, Serialize)]
struct ServeCycleReport {
    runtime_validation: git_relay::deploy::RuntimeValidationReport,
    processed_reconciles: Vec<ReconcileRunRecord>,
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
    let cli = DaemonCli::parse();
    match cli.command {
        DaemonCommand::Serve(args) => serve(args),
    }
}

fn serve(args: ServeArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let config = AppConfig::load(&args.config)?;
    let descriptors = config.load_repository_descriptors()?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);
    let report = validate_runtime_profile(&config, &descriptors, &validator)?;

    if !report.passed() {
        eprintln!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::from(1));
    }

    if args.once {
        let cycle = run_cycle(&config, &descriptors, report)?;
        println!("{}", serde_json::to_string_pretty(&cycle)?);
        return Ok(ExitCode::SUCCESS);
    }

    loop {
        let _ = run_cycle(&config, &descriptors, report.clone())?;
        thread::sleep(Duration::from_secs(60));
    }
}

fn run_cycle(
    config: &AppConfig,
    descriptors: &[git_relay::config::RepositoryDescriptor],
    runtime_validation: git_relay::deploy::RuntimeValidationReport,
) -> Result<ServeCycleReport, Box<dyn std::error::Error>> {
    let processed_reconciles = process_pending_reconcile_requests(config, descriptors)?;
    Ok(ServeCycleReport {
        runtime_validation,
        processed_reconciles,
    })
}
