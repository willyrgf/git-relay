use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::classification::{classify_startup, RepositorySafetyState};
use crate::config::{AppConfig, ConfigError, RepositoryDescriptor};
use crate::deploy::{
    render_service, validate_runtime_profile, ServiceFormat, ServiceRenderRequest,
};
use crate::git::SystemGitExecutor;
use crate::hooks::dispatch_hook_action;
use crate::platform::RealPlatformProbe;
use crate::read_path::{operator_prepare_repository_for_read, ReadPathError};
use crate::reconcile::{
    load_divergence_markers, reconcile_repository, replication_status_for_repo, ReconcileError,
};
use crate::upstream::{probe_repository_upstreams, UpstreamProbeError};
use crate::validator::{ValidationInfrastructureError, ValidationReport, Validator};

#[derive(Debug, Parser)]
#[command(name = "git-relay")]
#[command(about = "Git Relay control-plane bootstrap and validation CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: TopLevelCommand,
}

#[derive(Debug, Subcommand)]
enum TopLevelCommand {
    Deploy(DeployCommand),
    #[command(hide = true)]
    HookDispatch(HookDispatchCommand),
    Read(ReadCommand),
    Replication(ReplicationCommand),
    Repo(RepoCommand),
    Startup(StartupCommand),
}

#[derive(Debug, Args)]
struct DeployCommand {
    #[command(subcommand)]
    command: DeploySubcommand,
}

#[derive(Debug, Subcommand)]
enum DeploySubcommand {
    ValidateRuntime(TargetOptions),
    RenderService(RenderServiceOptions),
}

#[derive(Debug, Args)]
struct RepoCommand {
    #[command(subcommand)]
    command: RepoSubcommand,
}

#[derive(Debug, Args)]
struct ReplicationCommand {
    #[command(subcommand)]
    command: ReplicationSubcommand,
}

#[derive(Debug, Args)]
struct ReadCommand {
    #[command(subcommand)]
    command: ReadSubcommand,
}

#[derive(Debug, Subcommand)]
enum ReplicationSubcommand {
    ProbeUpstreams(TargetOptions),
    Reconcile(TargetOptions),
    Status(TargetOptions),
}

#[derive(Debug, Subcommand)]
enum ReadSubcommand {
    Prepare(TargetOptions),
}

#[derive(Debug, Subcommand)]
enum RepoSubcommand {
    Validate(TargetOptions),
}

#[derive(Debug, Args)]
struct StartupCommand {
    #[command(subcommand)]
    command: StartupSubcommand,
}

#[derive(Debug, Subcommand)]
enum StartupSubcommand {
    Classify(TargetOptions),
}

#[derive(Debug, Clone, Args)]
struct TargetOptions {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct RenderServiceOptions {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long, value_enum)]
    format: ServiceFormat,
    #[arg(long)]
    binary_path: std::path::PathBuf,
}

#[derive(Debug, Args)]
struct HookDispatchCommand {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long)]
    hook: String,
    #[arg(long)]
    repo: std::path::PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error(transparent)]
    UpstreamProbe(#[from] UpstreamProbeError),
    #[error(transparent)]
    ReadPath(#[from] ReadPathError),
    #[error(transparent)]
    ValidationInfrastructure(#[from] ValidationInfrastructureError),
    #[error("repository {0} was not found in the descriptor set")]
    RepositoryNotFound(String),
    #[error("failed to write command output: {0}")]
    Io(#[from] io::Error),
}

pub fn run<I, T>(args: I) -> Result<ExitCode, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    match cli.command {
        TopLevelCommand::Deploy(command) => match command.command {
            DeploySubcommand::ValidateRuntime(options) => run_deploy_validate_runtime(options),
            DeploySubcommand::RenderService(options) => run_deploy_render_service(options),
        },
        TopLevelCommand::HookDispatch(command) => run_hook_dispatch(command),
        TopLevelCommand::Read(command) => match command.command {
            ReadSubcommand::Prepare(options) => run_read_prepare(options),
        },
        TopLevelCommand::Replication(command) => match command.command {
            ReplicationSubcommand::ProbeUpstreams(options) => {
                run_replication_probe_upstreams(options)
            }
            ReplicationSubcommand::Reconcile(options) => run_replication_reconcile(options),
            ReplicationSubcommand::Status(options) => run_replication_status(options),
        },
        TopLevelCommand::Repo(command) => match command.command {
            RepoSubcommand::Validate(options) => run_repo_validate(options),
        },
        TopLevelCommand::Startup(command) => match command.command {
            StartupSubcommand::Classify(options) => run_startup_classify(options),
        },
    }
}

fn run_hook_dispatch(options: HookDispatchCommand) -> Result<ExitCode, CliError> {
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let event = dispatch_hook_action(
        &options.config,
        options.hook,
        options.repo,
        options.args,
        io::stdin().lock(),
        &git,
        &platform,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    if options.json {
        let mut stdout = io::BufWriter::new(io::stdout().lock());
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&event)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?
        )?;
        stdout.flush()?;
    }
    if event.accepted() {
        Ok(ExitCode::SUCCESS)
    } else {
        if let Some(message) = event.message {
            eprintln!("{message}");
        }
        Ok(ExitCode::from(1))
    }
}

fn run_deploy_validate_runtime(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);
    let report = validate_runtime_profile(&config, &targets, &validator)?;
    emit_output(&report, options.json)?;
    if report.passed() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run_replication_reconcile(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let reports = targets
        .iter()
        .map(|descriptor| reconcile_repository(&config, descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_replication_probe_upstreams(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let reports = targets
        .iter()
        .map(|descriptor| probe_repository_upstreams(&config, descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_replication_status(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let reports = targets
        .iter()
        .map(|descriptor| replication_status_for_repo(&config, descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_read_prepare(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let reports = targets
        .iter()
        .map(|descriptor| operator_prepare_repository_for_read(&config, descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_deploy_render_service(options: RenderServiceOptions) -> Result<ExitCode, CliError> {
    let config = AppConfig::load(&options.config)?;
    let rendered = render_service(
        &config,
        &ServiceRenderRequest {
            binary_path: options.binary_path,
            config_path: options.config,
            format: options.format,
        },
    );
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn run_repo_validate(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);
    let reports = targets
        .iter()
        .map(|descriptor| validator.validate(&config, descriptor))
        .collect::<Result<Vec<_>, _>>()?;

    emit_output(&reports, options.json)?;
    if reports.iter().all(ValidationReport::passed) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run_startup_classify(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);

    let mut classifications = Vec::new();
    let mut valid = true;
    for descriptor in targets {
        let report = validator.validate(&config, &descriptor)?;
        let mut classification = classify_startup(&descriptor, &report);
        if report.passed() {
            let divergence_markers = load_divergence_markers(&descriptor.repo_path)?;
            if !divergence_markers.is_empty() {
                classification.safety = RepositorySafetyState::Divergent;
                classification.write_acceptance_allowed = false;
            }
        }
        if !classification.write_acceptance_allowed {
            valid = false;
        }
        classifications.push(classification);
    }

    emit_output(&classifications, options.json)?;
    if valid {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn load_config_and_descriptors(
    path: &std::path::Path,
) -> Result<(AppConfig, Vec<RepositoryDescriptor>), CliError> {
    let config = AppConfig::load(path)?;
    let descriptors = config.load_repository_descriptors()?;
    Ok((config, descriptors))
}

fn select_repositories(
    descriptors: Vec<RepositoryDescriptor>,
    target_repo: Option<&str>,
) -> Result<Vec<RepositoryDescriptor>, CliError> {
    if let Some(target_repo) = target_repo {
        let descriptor = descriptors
            .into_iter()
            .find(|descriptor| descriptor.repo_id == target_repo)
            .ok_or_else(|| CliError::RepositoryNotFound(target_repo.to_owned()))?;
        Ok(vec![descriptor])
    } else {
        Ok(descriptors)
    }
}

fn emit_output<T: serde::Serialize>(payload: &T, json: bool) -> Result<(), CliError> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    if json {
        serde_json::to_writer_pretty(&mut stdout, payload)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        stdout.write_all(b"\n")?;
    } else {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(payload)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?
        )?;
    }
    stdout.flush()?;
    Ok(())
}
