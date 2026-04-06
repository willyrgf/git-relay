use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::audit::{new_structured_log_event, record_structured_log};
use crate::classification::{classify_startup, RepositorySafetyState, StartupClassification};
use crate::config::{AppConfig, ConfigError, RepositoryDescriptor};
use crate::deploy::{
    render_service, validate_runtime_profile, RuntimeValidationReport, ServiceFormat,
    ServiceRenderRequest,
};
use crate::git::SystemGitExecutor;
use crate::hooks::dispatch_hook_action;
use crate::migration::{
    inspect_migration, migrate_flake_inputs, parse_policy_overrides, MigrationError,
    MigrationRequest,
};
use crate::platform::RealPlatformProbe;
use crate::read_path::{operator_prepare_repository_for_read, ReadPathError};
use crate::reconcile::{
    load_divergence_markers, reconcile_repository, repair_repository, replication_status_for_repo,
    DivergenceMarker, ReconcileError, ReplicationStatus, RepoRepairReport,
};
use crate::release::{build_release_conformance_report, ReleaseError};
use crate::upstream::{
    build_release_manifest, probe_matrix_targets, probe_repository_upstreams, UpstreamProbeError,
};
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
    Doctor(TargetOptions),
    #[command(hide = true)]
    HookDispatch(HookDispatchCommand),
    Migration(MigrationCommand),
    MigrateFlakeInputs(MigrationApplyOptions),
    Read(ReadCommand),
    Release(ReleaseCommand),
    Replication(ReplicationCommand),
    Repo(RepoCommand),
    Startup(StartupCommand),
}

#[derive(Debug, Args)]
struct DeployCommand {
    #[command(subcommand)]
    command: DeploySubcommand,
}

#[derive(Debug, Args)]
struct ReleaseCommand {
    #[command(subcommand)]
    command: ReleaseSubcommand,
}

#[derive(Debug, Args)]
struct MigrationCommand {
    #[command(subcommand)]
    command: MigrationSubcommand,
}

#[derive(Debug, Subcommand)]
enum DeploySubcommand {
    ValidateRuntime(TargetOptions),
    RenderService(RenderServiceOptions),
}

#[derive(Debug, Subcommand)]
enum MigrationSubcommand {
    Inspect(MigrationInspectOptions),
}

#[derive(Debug, Subcommand)]
enum ReleaseSubcommand {
    Report(TargetOptions),
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
    BuildReleaseManifest(MatrixTargetOptions),
    ProbeMatrix(MatrixTargetOptions),
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
    Inspect(TargetOptions),
    Repair(TargetOptions),
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

#[derive(Debug, Clone, Args)]
struct MigrationPolicyOptions {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long, default_value = ".")]
    flake: std::path::PathBuf,
    #[arg(long = "input-target")]
    input_targets: Vec<String>,
    #[arg(long = "host-target")]
    host_targets: Vec<String>,
    #[arg(long = "class-target")]
    class_targets: Vec<String>,
    #[arg(long = "input-class")]
    input_classes: Vec<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
struct MigrationInspectOptions {
    #[command(flatten)]
    policy: MigrationPolicyOptions,
}

#[derive(Debug, Clone, Args)]
struct MigrationApplyOptions {
    #[command(flatten)]
    policy: MigrationPolicyOptions,
    #[arg(long)]
    allow_dirty: bool,
}

#[derive(Debug, Clone, Args)]
struct MatrixTargetOptions {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long)]
    repo: String,
    #[arg(long)]
    targets: std::path::PathBuf,
    #[arg(long)]
    json: bool,
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
    Migration(#[from] MigrationError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error(transparent)]
    UpstreamProbe(#[from] UpstreamProbeError),
    #[error(transparent)]
    ReadPath(#[from] ReadPathError),
    #[error(transparent)]
    ValidationInfrastructure(#[from] ValidationInfrastructureError),
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error("repository {0} was not found in the descriptor set")]
    RepositoryNotFound(String),
    #[error("failed to write command output: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, serde::Serialize)]
struct RepoInspectionReport {
    descriptor: RepositoryDescriptor,
    validation: ValidationReport,
    startup: StartupClassification,
    replication: ReplicationStatus,
    divergence_markers: Vec<DivergenceMarker>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RepoRepairEnvelope {
    descriptor: RepositoryDescriptor,
    repair: RepoRepairReport,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DoctorReport {
    runtime_validation: RuntimeValidationReport,
    repositories: Vec<RepoInspectionReport>,
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
        TopLevelCommand::Doctor(options) => run_doctor(options),
        TopLevelCommand::HookDispatch(command) => run_hook_dispatch(command),
        TopLevelCommand::Migration(command) => match command.command {
            MigrationSubcommand::Inspect(options) => run_migration_inspect(options),
        },
        TopLevelCommand::MigrateFlakeInputs(options) => run_migrate_flake_inputs(options),
        TopLevelCommand::Read(command) => match command.command {
            ReadSubcommand::Prepare(options) => run_read_prepare(options),
        },
        TopLevelCommand::Release(command) => match command.command {
            ReleaseSubcommand::Report(options) => run_release_report(options),
        },
        TopLevelCommand::Replication(command) => match command.command {
            ReplicationSubcommand::BuildReleaseManifest(options) => {
                run_replication_build_release_manifest(options)
            }
            ReplicationSubcommand::ProbeMatrix(options) => run_replication_probe_matrix(options),
            ReplicationSubcommand::ProbeUpstreams(options) => {
                run_replication_probe_upstreams(options)
            }
            ReplicationSubcommand::Reconcile(options) => run_replication_reconcile(options),
            ReplicationSubcommand::Status(options) => run_replication_status(options),
        },
        TopLevelCommand::Repo(command) => match command.command {
            RepoSubcommand::Inspect(options) => run_repo_inspect(options),
            RepoSubcommand::Repair(options) => run_repo_repair(options),
            RepoSubcommand::Validate(options) => run_repo_validate(options),
        },
        TopLevelCommand::Startup(command) => match command.command {
            StartupSubcommand::Classify(options) => run_startup_classify(options),
        },
    }
}

fn run_migration_inspect(options: MigrationInspectOptions) -> Result<ExitCode, CliError> {
    let config = AppConfig::load(&options.policy.config)?;
    let request = build_migration_request(&options.policy, false)?;
    let report = inspect_migration(&config, &request)?;
    record_cli_command_event(
        &config,
        "migration.inspect",
        None,
        serde_json::json!({
            "flake": request.flake_path,
            "planned_rewrite_count": report.planned_rewrites.len(),
            "unresolved_transitive_shorthand_count": report.unresolved_transitive_shorthand.len(),
        }),
    );
    emit_output(&report, options.policy.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_migrate_flake_inputs(options: MigrationApplyOptions) -> Result<ExitCode, CliError> {
    let config = AppConfig::load(&options.policy.config)?;
    let request = build_migration_request(&options.policy, options.allow_dirty)?;
    let report = migrate_flake_inputs(&config, &request)?;
    record_cli_command_event(
        &config,
        "migrate-flake-inputs",
        None,
        serde_json::json!({
            "flake": request.flake_path,
            "relocked_inputs": report.relocked_inputs.clone(),
            "planned_rewrite_count": report.planned_rewrites.len(),
        }),
    );
    emit_output(&report, options.policy.json)?;
    Ok(ExitCode::SUCCESS)
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
    record_cli_command_event(
        &config,
        "replication.reconcile",
        options.repo.as_deref(),
        serde_json::json!({
            "run_count": reports.len(),
        }),
    );
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_release_report(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let report = build_release_conformance_report(&config, &descriptors, options.repo.as_deref())?;
    record_cli_command_event(
        &config,
        "release.report",
        options.repo.as_deref(),
        serde_json::json!({
            "repo_manifest_count": report.repo_manifests.len(),
            "exact_git_floor_status": report.exact_git_floor_status,
            "exact_nix_floor_status": report.exact_nix_floor_status,
        }),
    );
    emit_output(&report, options.json)?;
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

fn run_replication_probe_matrix(options: MatrixTargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let target = select_repositories(descriptors, Some(&options.repo))?
        .into_iter()
        .next()
        .ok_or_else(|| CliError::RepositoryNotFound(options.repo.clone()))?;
    let report = probe_matrix_targets(&config, &target, &options.targets)?;
    emit_output(&report, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_replication_build_release_manifest(
    options: MatrixTargetOptions,
) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let target = select_repositories(descriptors, Some(&options.repo))?
        .into_iter()
        .next()
        .ok_or_else(|| CliError::RepositoryNotFound(options.repo.clone()))?;
    let manifest = build_release_manifest(&config, &target, &options.targets)?;
    let all_entries_admitted = manifest.all_entries_admitted;
    emit_output(&manifest, options.json)?;
    if all_entries_admitted {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
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

fn run_doctor(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let target_count = targets.len();
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);
    let runtime_validation = validate_runtime_profile(&config, &targets, &validator)?;
    let repositories = build_repo_inspections(&config, targets, &validator)?;
    let passed = runtime_validation.passed()
        && repositories.iter().all(|report| {
            report.validation.passed()
                && !matches!(
                    report.startup.safety,
                    RepositorySafetyState::Divergent | RepositorySafetyState::Quarantined
                )
        });
    emit_output(
        &DoctorReport {
            runtime_validation,
            repositories,
        },
        options.json,
    )?;
    record_cli_command_event(
        &config,
        "doctor",
        options.repo.as_deref(),
        serde_json::json!({
            "repository_count": target_count,
        }),
    );
    if passed {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
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

fn run_repo_inspect(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let git = SystemGitExecutor;
    let platform = RealPlatformProbe;
    let validator = Validator::new(&git, &platform);
    let reports = build_repo_inspections(&config, targets, &validator)?;
    record_cli_command_event(
        &config,
        "repo.inspect",
        options.repo.as_deref(),
        serde_json::json!({
            "repository_count": reports.len(),
        }),
    );
    emit_output(&reports, options.json)?;
    Ok(ExitCode::SUCCESS)
}

fn run_repo_repair(options: TargetOptions) -> Result<ExitCode, CliError> {
    let (config, descriptors) = load_config_and_descriptors(&options.config)?;
    let targets = select_repositories(descriptors, options.repo.as_deref())?;
    let reports = targets
        .into_iter()
        .map(|descriptor| {
            let repair = repair_repository(&config, &descriptor)?;
            Ok(RepoRepairEnvelope { descriptor, repair })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    record_cli_command_event(
        &config,
        "repo.repair",
        options.repo.as_deref(),
        serde_json::json!({
            "repository_count": reports.len(),
        }),
    );
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

fn build_repo_inspections<G, P>(
    config: &AppConfig,
    descriptors: Vec<RepositoryDescriptor>,
    validator: &Validator<'_, G, P>,
) -> Result<Vec<RepoInspectionReport>, CliError>
where
    G: crate::git::GitExecutor,
    P: crate::platform::PlatformProbe,
{
    descriptors
        .into_iter()
        .map(|descriptor| build_repo_inspection(config, descriptor, validator))
        .collect()
}

fn build_repo_inspection<G, P>(
    config: &AppConfig,
    descriptor: RepositoryDescriptor,
    validator: &Validator<'_, G, P>,
) -> Result<RepoInspectionReport, CliError>
where
    G: crate::git::GitExecutor,
    P: crate::platform::PlatformProbe,
{
    let validation = validator.validate(config, &descriptor)?;
    let divergence_markers = load_divergence_markers(&descriptor.repo_path)?;
    let mut startup = classify_startup(&descriptor, &validation);
    if validation.passed() && !divergence_markers.is_empty() {
        startup.safety = RepositorySafetyState::Divergent;
        startup.write_acceptance_allowed = false;
    }
    let replication = replication_status_for_repo(config, &descriptor)?;

    Ok(RepoInspectionReport {
        descriptor,
        validation,
        startup,
        replication,
        divergence_markers,
    })
}

fn build_migration_request(
    options: &MigrationPolicyOptions,
    allow_dirty: bool,
) -> Result<MigrationRequest, CliError> {
    let policy = parse_policy_overrides(
        &options.input_targets,
        &options.host_targets,
        &options.class_targets,
        &options.input_classes,
    )?;
    Ok(MigrationRequest {
        flake_path: options.flake.clone(),
        allow_dirty,
        policy,
    })
}

fn record_cli_command_event(
    config: &AppConfig,
    command_name: &str,
    repo_id: Option<&str>,
    payload: serde_json::Value,
) {
    let mut event = new_structured_log_event("cli.command");
    event.repo_id = repo_id.map(str::to_owned);
    event.payload = serde_json::json!({
        "command": command_name,
        "details": payload,
    });
    let _ = record_structured_log(&config.paths.state_root, &event);
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
