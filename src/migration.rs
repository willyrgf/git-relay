use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::config::{AppConfig, MigrationTransport};

const SUPPORTED_NIX_VERSIONS: &[&str] = &[
    "nix (Determinate Nix 3.0.0) 2.26.3",
    "nix (Nix) 2.28.5",
    "nix (Nix) 2.30.3+2",
    "nix (Nix) 2.31.3",
];

pub fn validated_targeted_relock_nix_versions() -> &'static [&'static str] {
    SUPPORTED_NIX_VERSIONS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationRepoClass {
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationPolicySelection {
    pub input_targets: BTreeMap<String, MigrationTransport>,
    pub host_targets: BTreeMap<String, MigrationTransport>,
    pub class_targets: BTreeMap<MigrationRepoClass, MigrationTransport>,
    pub input_classes: BTreeMap<String, MigrationRepoClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRequest {
    pub flake_path: PathBuf,
    pub allow_dirty: bool,
    pub policy: MigrationPolicySelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectInputState {
    PlannedRewrite,
    AlreadyGitTransport,
    OtherLiteral,
    BlockedNoPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectInputReport {
    pub input_name: String,
    pub original_url: String,
    pub shorthand_kind: Option<String>,
    pub source_host: Option<String>,
    pub repo_class: Option<MigrationRepoClass>,
    pub state: DirectInputState,
    pub selected_target: Option<MigrationTransport>,
    pub rewritten_url: Option<String>,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRewrite {
    pub input_name: String,
    pub before_url: String,
    pub after_url: String,
    pub target: MigrationTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitiveShorthandNode {
    pub node_id: String,
    pub shorthand_type: String,
    pub owner: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationInspectReport {
    pub flake_file: PathBuf,
    pub lock_file: PathBuf,
    pub direct_inputs: Vec<DirectInputReport>,
    pub planned_rewrites: Vec<PlannedRewrite>,
    pub unresolved_transitive_shorthand: Vec<TransitiveShorthandNode>,
    pub preview_diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationApplyReport {
    pub flake_file: PathBuf,
    pub lock_file: PathBuf,
    pub nix_version: Option<String>,
    pub direct_inputs: Vec<DirectInputReport>,
    pub planned_rewrites: Vec<PlannedRewrite>,
    pub relocked_inputs: Vec<String>,
    pub unresolved_transitive_shorthand: Vec<TransitiveShorthandNode>,
    pub diff: String,
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("failed to read {path}: {error}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write {path}: {error}", path = path.display())]
    Write {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to parse JSON {path}: {error}", path = path.display())]
    ParseJson {
        path: PathBuf,
        #[source]
        error: serde_json::Error,
    },
    #[error("failed to resolve migration path {path}: {error}", path = path.display())]
    Canonicalize {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("flake.nix was not found at {0}")]
    MissingFlake(PathBuf),
    #[error("flake.lock was not found at {0}")]
    MissingLock(PathBuf),
    #[error("unsupported direct input expression for {input_name}: {detail}")]
    UnsupportedExpression { input_name: String, detail: String },
    #[error("unsupported shorthand url for {input_name}: {detail}")]
    UnsupportedShorthand { input_name: String, detail: String },
    #[error("no migration target policy matched direct input {input_name}")]
    NoTargetPolicy { input_name: String },
    #[error("migration refused because the git worktree at {worktree_root} is dirty")]
    DirtyWorktree { worktree_root: PathBuf },
    #[error("nix version {version} is outside the validated targeted relock matrix")]
    UnsupportedNixVersion { version: String },
    #[error("flake.lock at {path} is outside the validated graph contract: {detail}", path = path.display())]
    InvalidLockGraph { path: PathBuf, detail: String },
    #[error(
        "targeted relock for {input_name} changed nodes outside the selected input closure: {detail}"
    )]
    RelockScopeViolation { input_name: String, detail: String },
    #[error("targeted relock for {input_name} was not idempotent on the second run")]
    RelockNotIdempotent { input_name: String },
    #[error(
        "migration cannot proceed because direct shorthand inputs remain without policy: {inputs}"
    )]
    BlockedDirectInputs { inputs: String },
    #[error("failed to parse migration policy option {option_kind}={value}")]
    InvalidPolicyOption { option_kind: String, value: String },
    #[error("failed to spawn {program} for args {args:?}: {error}")]
    SpawnCommand {
        program: String,
        args: Vec<String>,
        #[source]
        error: std::io::Error,
    },
    #[error("{program} failed for args {args:?} with status {status:?}: {detail}")]
    Command {
        program: String,
        args: Vec<String>,
        status: Option<i32>,
        detail: String,
    },
}

pub fn parse_policy_overrides(
    input_targets: &[String],
    host_targets: &[String],
    class_targets: &[String],
    input_classes: &[String],
) -> Result<MigrationPolicySelection, MigrationError> {
    let mut policy = MigrationPolicySelection::default();

    for entry in input_targets {
        let (input_name, target) = parse_key_value(entry, "input-target")?;
        policy.input_targets.insert(
            input_name.to_owned(),
            parse_transport(target, "input-target")?,
        );
    }
    for entry in host_targets {
        let (host, target) = parse_key_value(entry, "host-target")?;
        policy.host_targets.insert(
            host.to_ascii_lowercase(),
            parse_transport(target, "host-target")?,
        );
    }
    for entry in class_targets {
        let (class, target) = parse_key_value(entry, "class-target")?;
        policy.class_targets.insert(
            parse_repo_class(class, "class-target")?,
            parse_transport(target, "class-target")?,
        );
    }
    for entry in input_classes {
        let (input_name, class) = parse_key_value(entry, "input-class")?;
        policy.input_classes.insert(
            input_name.to_owned(),
            parse_repo_class(class, "input-class")?,
        );
    }

    Ok(policy)
}

pub fn inspect_migration(
    config: &AppConfig,
    request: &MigrationRequest,
) -> Result<MigrationInspectReport, MigrationError> {
    let project = load_flake_project(&request.flake_path)?;
    let flake_source =
        fs::read_to_string(&project.flake_file).map_err(|error| MigrationError::Read {
            path: project.flake_file.clone(),
            error,
        })?;
    let lock = load_lock_file(&project.lock_file)?;
    let assignments = parse_direct_input_assignments(&flake_source)?;
    ensure_supported_direct_coverage(&project.lock_file, &lock, &assignments)?;
    let (direct_inputs, planned_rewrites, rewritten_source) =
        build_rewrite_plan(config, request, &flake_source, &assignments)?;

    let preview_diff = render_diff(
        "flake.nix.before",
        &flake_source,
        "flake.nix.after",
        &rewritten_source,
    )?;
    let unresolved_transitive_shorthand = collect_transitive_shorthand(&project.lock_file, &lock)?;

    Ok(MigrationInspectReport {
        flake_file: project.flake_file,
        lock_file: project.lock_file,
        direct_inputs,
        planned_rewrites,
        unresolved_transitive_shorthand,
        preview_diff,
    })
}

pub fn migrate_flake_inputs(
    config: &AppConfig,
    request: &MigrationRequest,
) -> Result<MigrationApplyReport, MigrationError> {
    let project = load_flake_project(&request.flake_path)?;
    if config.migration.refuse_dirty_worktree && !request.allow_dirty {
        ensure_clean_worktree(&project.flake_dir)?;
    }

    let original_flake_source =
        fs::read_to_string(&project.flake_file).map_err(|error| MigrationError::Read {
            path: project.flake_file.clone(),
            error,
        })?;
    let original_lock_source =
        fs::read_to_string(&project.lock_file).map_err(|error| MigrationError::Read {
            path: project.lock_file.clone(),
            error,
        })?;
    let original_lock = load_lock_file(&project.lock_file)?;
    let assignments = parse_direct_input_assignments(&original_flake_source)?;
    ensure_supported_direct_coverage(&project.lock_file, &original_lock, &assignments)?;
    let (direct_inputs, planned_rewrites, rewritten_source) =
        build_rewrite_plan(config, request, &original_flake_source, &assignments)?;

    let blocked_inputs = direct_inputs
        .iter()
        .filter(|item| item.state == DirectInputState::BlockedNoPolicy)
        .map(|item| item.input_name.clone())
        .collect::<Vec<_>>();
    if !blocked_inputs.is_empty() {
        return Err(MigrationError::BlockedDirectInputs {
            inputs: blocked_inputs.join(", "),
        });
    }

    if planned_rewrites.is_empty() {
        let unresolved_transitive_shorthand =
            collect_transitive_shorthand(&project.lock_file, &original_lock)?;
        return Ok(MigrationApplyReport {
            flake_file: project.flake_file,
            lock_file: project.lock_file,
            nix_version: None,
            direct_inputs,
            planned_rewrites,
            relocked_inputs: Vec::new(),
            unresolved_transitive_shorthand,
            diff: String::new(),
        });
    }

    let nix_version = read_nix_version()?;
    if !SUPPORTED_NIX_VERSIONS
        .iter()
        .any(|allowed| *allowed == nix_version)
    {
        return Err(MigrationError::UnsupportedNixVersion {
            version: nix_version,
        });
    }

    fs::write(&project.flake_file, &rewritten_source).map_err(|error| MigrationError::Write {
        path: project.flake_file.clone(),
        error,
    })?;

    let mut relocked_inputs = Vec::new();
    let apply_result = (|| -> Result<MigrationApplyReport, MigrationError> {
        for rewrite in &planned_rewrites {
            validate_targeted_relock_scope(&project.lock_file, &rewrite.input_name)?;
            let before_second =
                fs::read_to_string(&project.lock_file).map_err(|error| MigrationError::Read {
                    path: project.lock_file.clone(),
                    error,
                })?;
            run_nix_flake_update(&project.flake_dir, &rewrite.input_name)?;
            validate_targeted_relock_result(
                &project.lock_file,
                &rewrite.input_name,
                &before_second,
            )?;

            let after_first =
                fs::read_to_string(&project.lock_file).map_err(|error| MigrationError::Read {
                    path: project.lock_file.clone(),
                    error,
                })?;
            run_nix_flake_update(&project.flake_dir, &rewrite.input_name)?;
            let after_second =
                fs::read_to_string(&project.lock_file).map_err(|error| MigrationError::Read {
                    path: project.lock_file.clone(),
                    error,
                })?;
            if after_first != after_second {
                return Err(MigrationError::RelockNotIdempotent {
                    input_name: rewrite.input_name.clone(),
                });
            }
            relocked_inputs.push(rewrite.input_name.clone());
        }

        let final_flake_source =
            fs::read_to_string(&project.flake_file).map_err(|error| MigrationError::Read {
                path: project.flake_file.clone(),
                error,
            })?;
        let final_lock_source =
            fs::read_to_string(&project.lock_file).map_err(|error| MigrationError::Read {
                path: project.lock_file.clone(),
                error,
            })?;
        let final_lock = load_lock_file(&project.lock_file)?;
        let diff = render_combined_diff(
            &original_flake_source,
            &final_flake_source,
            &original_lock_source,
            &final_lock_source,
        )?;

        Ok(MigrationApplyReport {
            flake_file: project.flake_file.clone(),
            lock_file: project.lock_file.clone(),
            nix_version: Some(nix_version),
            direct_inputs,
            planned_rewrites,
            relocked_inputs,
            unresolved_transitive_shorthand: collect_transitive_shorthand(
                &project.lock_file,
                &final_lock,
            )?,
            diff,
        })
    })();

    if apply_result.is_err() {
        let _ = fs::write(&project.flake_file, original_flake_source);
        let _ = fs::write(&project.lock_file, original_lock_source);
    }

    apply_result
}

fn build_rewrite_plan(
    config: &AppConfig,
    request: &MigrationRequest,
    flake_source: &str,
    assignments: &[DirectInputAssignment],
) -> Result<(Vec<DirectInputReport>, Vec<PlannedRewrite>, String), MigrationError> {
    let mut direct_inputs = Vec::new();
    let mut planned_rewrites = Vec::new();
    let mut rewritten = flake_source.to_owned();

    let mut replacements = Vec::new();
    for assignment in assignments {
        let report = classify_assignment(config, request, assignment)?;
        if let Some(after_url) = &report.rewritten_url {
            replacements.push((
                assignment.value_span,
                after_url.clone(),
                report.input_name.clone(),
            ));
            planned_rewrites.push(PlannedRewrite {
                input_name: report.input_name.clone(),
                before_url: report.original_url.clone(),
                after_url: after_url.clone(),
                target: report.selected_target.expect("planned rewrite target"),
            });
        }
        direct_inputs.push(report);
    }

    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (span, after_url, _input_name) in replacements {
        rewritten.replace_range(span.start..span.end, &quote_string(&after_url));
    }

    Ok((direct_inputs, planned_rewrites, rewritten))
}

fn classify_assignment(
    config: &AppConfig,
    request: &MigrationRequest,
    assignment: &DirectInputAssignment,
) -> Result<DirectInputReport, MigrationError> {
    let original_url = assignment.value.clone();
    if original_url.starts_with("git+https://") || original_url.starts_with("git+ssh://") {
        return Ok(DirectInputReport {
            input_name: assignment.input_name.clone(),
            original_url,
            shorthand_kind: None,
            source_host: None,
            repo_class: request
                .policy
                .input_classes
                .get(&assignment.input_name)
                .copied(),
            state: DirectInputState::AlreadyGitTransport,
            selected_target: None,
            rewritten_url: None,
            blocked_reason: None,
        });
    }
    if !matches_shorthand_prefix(&original_url) {
        return Ok(DirectInputReport {
            input_name: assignment.input_name.clone(),
            original_url,
            shorthand_kind: None,
            source_host: None,
            repo_class: request
                .policy
                .input_classes
                .get(&assignment.input_name)
                .copied(),
            state: DirectInputState::OtherLiteral,
            selected_target: None,
            rewritten_url: None,
            blocked_reason: None,
        });
    }

    let shorthand = parse_shorthand_url(&assignment.input_name, &original_url)?;
    let repo_class = request
        .policy
        .input_classes
        .get(&assignment.input_name)
        .copied();
    let selected_target = select_transport(
        config,
        request,
        &assignment.input_name,
        &shorthand.host,
        repo_class,
    );
    match selected_target {
        Some(target) => Ok(DirectInputReport {
            input_name: assignment.input_name.clone(),
            original_url,
            shorthand_kind: Some(shorthand.kind.label().to_owned()),
            source_host: Some(shorthand.host.clone()),
            repo_class,
            state: DirectInputState::PlannedRewrite,
            selected_target: Some(target),
            rewritten_url: Some(render_git_url(&shorthand, target)),
            blocked_reason: None,
        }),
        None => Ok(DirectInputReport {
            input_name: assignment.input_name.clone(),
            original_url,
            shorthand_kind: Some(shorthand.kind.label().to_owned()),
            source_host: Some(shorthand.host.clone()),
            repo_class,
            state: DirectInputState::BlockedNoPolicy,
            selected_target: None,
            rewritten_url: None,
            blocked_reason: Some("no migration target policy matched this direct input".to_owned()),
        }),
    }
}

fn select_transport(
    config: &AppConfig,
    request: &MigrationRequest,
    input_name: &str,
    host: &str,
    repo_class: Option<MigrationRepoClass>,
) -> Option<MigrationTransport> {
    if let Some(target) = request.policy.input_targets.get(input_name) {
        return Some(*target);
    }
    if let Some(target) = request.policy.host_targets.get(&host.to_ascii_lowercase()) {
        return Some(*target);
    }
    if let Some(repo_class) = repo_class {
        if let Some(target) = request.policy.class_targets.get(&repo_class) {
            return Some(*target);
        }
    }
    if config.migration.supported_targets.len() == 1 {
        return config.migration.supported_targets.first().copied();
    }
    None
}

fn load_flake_project(path: &Path) -> Result<FlakeProject, MigrationError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| MigrationError::Canonicalize {
            path: path.to_path_buf(),
            error,
        })?;
    let (flake_dir, flake_file) = if canonical.is_dir() {
        (canonical.clone(), canonical.join("flake.nix"))
    } else {
        let file_name = canonical.file_name().and_then(|value| value.to_str());
        if file_name != Some("flake.nix") {
            return Err(MigrationError::MissingFlake(canonical));
        }
        (
            canonical.parent().unwrap_or(Path::new(".")).to_path_buf(),
            canonical.clone(),
        )
    };
    if !flake_file.exists() {
        return Err(MigrationError::MissingFlake(flake_file));
    }
    let lock_file = flake_dir.join("flake.lock");
    if !lock_file.exists() {
        return Err(MigrationError::MissingLock(lock_file));
    }
    Ok(FlakeProject {
        flake_dir,
        flake_file,
        lock_file,
    })
}

fn parse_direct_input_assignments(
    source: &str,
) -> Result<Vec<DirectInputAssignment>, MigrationError> {
    let mut parser = FlakeParser::new(source);
    parser.parse()
}

fn parse_shorthand_url(
    input_name: &str,
    value: &str,
) -> Result<ParsedShorthandUrl, MigrationError> {
    let (scheme, remainder) =
        value
            .split_once(':')
            .ok_or_else(|| MigrationError::UnsupportedShorthand {
                input_name: input_name.to_owned(),
                detail: "missing shorthand scheme".to_owned(),
            })?;
    let (path_part, query_part) = match remainder.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (remainder, None),
    };

    let mut query = ParsedShorthandQuery::default();
    if let Some(query_part) = query_part {
        for pair in query_part.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) =
                pair.split_once('=')
                    .ok_or_else(|| MigrationError::UnsupportedShorthand {
                        input_name: input_name.to_owned(),
                        detail: format!("query parameter {pair} is missing '='"),
                    })?;
            let slot = match key {
                "dir" => &mut query.dir,
                "ref" => &mut query.ref_name,
                "rev" => &mut query.rev,
                "host" => &mut query.host,
                _ => {
                    return Err(MigrationError::UnsupportedShorthand {
                        input_name: input_name.to_owned(),
                        detail: format!("unsupported query parameter {key}"),
                    })
                }
            };
            if slot.replace(value.to_owned()).is_some() {
                return Err(MigrationError::UnsupportedShorthand {
                    input_name: input_name.to_owned(),
                    detail: format!("duplicate query parameter {key}"),
                });
            }
        }
    }

    match scheme {
        "github" => parse_github_shorthand(input_name, path_part, query),
        "gitlab" => parse_gitlab_shorthand(input_name, path_part, query),
        "sourcehut" => parse_sourcehut_shorthand(input_name, path_part, query),
        _ => Err(MigrationError::UnsupportedShorthand {
            input_name: input_name.to_owned(),
            detail: format!("unsupported shorthand scheme {scheme}"),
        }),
    }
}

fn parse_github_shorthand(
    input_name: &str,
    path_part: &str,
    query: ParsedShorthandQuery,
) -> Result<ParsedShorthandUrl, MigrationError> {
    let segments = path_part
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if !(segments.len() == 2 || segments.len() == 3) {
        return Err(MigrationError::UnsupportedShorthand {
            input_name: input_name.to_owned(),
            detail: "github shorthand must be owner/repo or owner/repo/ref".to_owned(),
        });
    }
    if segments.len() == 3 && query.ref_name.is_some() {
        return Err(MigrationError::UnsupportedShorthand {
            input_name: input_name.to_owned(),
            detail: "github shorthand path ref and query ref are ambiguous together".to_owned(),
        });
    }
    Ok(ParsedShorthandUrl {
        kind: ShorthandKind::Github,
        host: query.host.unwrap_or_else(|| "github.com".to_owned()),
        repo_path: format!("{}/{}", segments[0], segments[1]),
        ref_name: query
            .ref_name
            .or_else(|| segments.get(2).map(|segment| (*segment).to_owned())),
        rev: query.rev,
        dir: query.dir,
    })
}

fn parse_gitlab_shorthand(
    input_name: &str,
    path_part: &str,
    query: ParsedShorthandQuery,
) -> Result<ParsedShorthandUrl, MigrationError> {
    let segments = path_part
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(MigrationError::UnsupportedShorthand {
            input_name: input_name.to_owned(),
            detail: "gitlab shorthand must include at least group/project".to_owned(),
        });
    }
    Ok(ParsedShorthandUrl {
        kind: ShorthandKind::Gitlab,
        host: query.host.unwrap_or_else(|| "gitlab.com".to_owned()),
        repo_path: segments.join("/"),
        ref_name: query.ref_name,
        rev: query.rev,
        dir: query.dir,
    })
}

fn parse_sourcehut_shorthand(
    input_name: &str,
    path_part: &str,
    query: ParsedShorthandQuery,
) -> Result<ParsedShorthandUrl, MigrationError> {
    let segments = path_part
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() != 2 || !segments[0].starts_with('~') {
        return Err(MigrationError::UnsupportedShorthand {
            input_name: input_name.to_owned(),
            detail: "sourcehut shorthand must be ~user/project".to_owned(),
        });
    }
    Ok(ParsedShorthandUrl {
        kind: ShorthandKind::Sourcehut,
        host: query.host.unwrap_or_else(|| "git.sr.ht".to_owned()),
        repo_path: format!("{}/{}", segments[0], segments[1]),
        ref_name: query.ref_name,
        rev: query.rev,
        dir: query.dir,
    })
}

fn render_git_url(value: &ParsedShorthandUrl, target: MigrationTransport) -> String {
    let mut url = match target {
        MigrationTransport::GitHttps => format!("git+https://{}/{}", value.host, value.repo_path),
        MigrationTransport::GitSsh => format!("git+ssh://git@{}/{}", value.host, value.repo_path),
    };

    let mut query = Vec::new();
    if let Some(dir) = &value.dir {
        query.push(format!("dir={dir}"));
    }
    if let Some(ref_name) = &value.ref_name {
        query.push(format!("ref={ref_name}"));
    }
    if let Some(rev) = &value.rev {
        query.push(format!("rev={rev}"));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

fn load_lock_file(path: &Path) -> Result<FlakeLock, MigrationError> {
    let source = fs::read_to_string(path).map_err(|error| MigrationError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    serde_json::from_str(&source).map_err(|error| MigrationError::ParseJson {
        path: path.to_path_buf(),
        error,
    })
}

fn validate_targeted_relock_scope(
    lock_path: &Path,
    input_name: &str,
) -> Result<(), MigrationError> {
    let lock = load_lock_file(lock_path)?;
    validate_lock_graph(lock_path, &lock)?;
    ensure_root_input_exists(lock_path, &lock, input_name)?;
    Ok(())
}

fn validate_targeted_relock_result(
    lock_path: &Path,
    input_name: &str,
    before_source: &str,
) -> Result<(), MigrationError> {
    let before = serde_json::from_str::<FlakeLock>(before_source).map_err(|error| {
        MigrationError::ParseJson {
            path: lock_path.to_path_buf(),
            error,
        }
    })?;
    let after = load_lock_file(lock_path)?;
    validate_lock_graph(lock_path, &before)?;
    validate_lock_graph(lock_path, &after)?;

    let allowed = allowed_changed_nodes(lock_path, &before, input_name)?;
    let changed = changed_lock_nodes(&before, &after);
    let disallowed = changed.difference(&allowed).cloned().collect::<Vec<_>>();
    if !disallowed.is_empty() {
        return Err(MigrationError::RelockScopeViolation {
            input_name: input_name.to_owned(),
            detail: format!(
                "changed nodes [{}], allowed nodes [{}]",
                disallowed.join(", "),
                allowed.into_iter().collect::<Vec<_>>().join(", ")
            ),
        });
    }

    Ok(())
}

fn collect_transitive_shorthand(
    lock_path: &Path,
    lock: &FlakeLock,
) -> Result<Vec<TransitiveShorthandNode>, MigrationError> {
    validate_lock_graph(lock_path, lock)?;

    let root_node = lock
        .nodes
        .get(&lock.root)
        .ok_or_else(|| MigrationError::InvalidLockGraph {
            path: lock_path.to_path_buf(),
            detail: format!("root node {} is missing", lock.root),
        })?;
    let direct_root_targets = root_node
        .inputs
        .values()
        .map(|value| {
            resolve_input_reference(lock, &lock.root, value).map_err(|detail| {
                MigrationError::InvalidLockGraph {
                    path: lock_path.to_path_buf(),
                    detail: format!("root input reference: {detail}"),
                }
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;

    let mut nodes = Vec::new();
    for (node_id, node) in &lock.nodes {
        if node_id == &lock.root || direct_root_targets.contains(node_id) {
            continue;
        }
        let shorthand_type = shorthand_type_from_node(node);
        if let Some(shorthand_type) = shorthand_type {
            nodes.push(TransitiveShorthandNode {
                node_id: node_id.clone(),
                shorthand_type: shorthand_type.to_owned(),
                owner: node
                    .original
                    .as_ref()
                    .and_then(|value| value.get("owner"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                repo: node
                    .original
                    .as_ref()
                    .and_then(|value| value.get("repo"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            });
        }
    }
    nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    Ok(nodes)
}

fn ensure_supported_direct_coverage(
    lock_path: &Path,
    lock: &FlakeLock,
    assignments: &[DirectInputAssignment],
) -> Result<(), MigrationError> {
    validate_lock_graph(lock_path, lock)?;

    let root_node = lock
        .nodes
        .get(&lock.root)
        .ok_or_else(|| MigrationError::InvalidLockGraph {
            path: lock_path.to_path_buf(),
            detail: format!("root node {} is missing", lock.root),
        })?;
    let parsed_inputs = assignments
        .iter()
        .map(|assignment| assignment.input_name.as_str())
        .collect::<BTreeSet<_>>();

    for (input_name, input_ref) in &root_node.inputs {
        let Some(target_node_id) = input_ref.as_str() else {
            continue;
        };
        let Some(target_node) = lock.nodes.get(target_node_id) else {
            return Err(MigrationError::InvalidLockGraph {
                path: lock_path.to_path_buf(),
                detail: format!("root input {input_name} points to missing node {target_node_id}"),
            });
        };
        if shorthand_type_from_node(target_node).is_some()
            && !parsed_inputs.contains(input_name.as_str())
        {
            return Err(MigrationError::UnsupportedExpression {
                input_name: input_name.clone(),
                detail: "direct shorthand input is outside the supported literal grammar; expected inputs.<name>.url = \"...\"".to_owned(),
            });
        }
    }

    Ok(())
}

fn shorthand_type_from_node(node: &FlakeLockNode) -> Option<&'static str> {
    for value in [&node.original, &node.locked] {
        let shorthand_type = value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str());
        match shorthand_type {
            Some("github") => return Some("github"),
            Some("gitlab") => return Some("gitlab"),
            Some("sourcehut") => return Some("sourcehut"),
            _ => {}
        }
    }
    None
}

fn validate_lock_graph(path: &Path, lock: &FlakeLock) -> Result<(), MigrationError> {
    if lock.version != 7 {
        return Err(MigrationError::InvalidLockGraph {
            path: path.to_path_buf(),
            detail: format!("unsupported lock version {}; expected 7", lock.version),
        });
    }
    if !lock.nodes.contains_key(&lock.root) {
        return Err(MigrationError::InvalidLockGraph {
            path: path.to_path_buf(),
            detail: format!("root node {} is missing", lock.root),
        });
    }

    for (node_id, node) in &lock.nodes {
        for (input_name, input_ref) in &node.inputs {
            resolve_input_reference(lock, node_id, input_ref).map_err(|detail| {
                MigrationError::InvalidLockGraph {
                    path: path.to_path_buf(),
                    detail: format!("node {node_id} input {input_name}: {detail}"),
                }
            })?;
        }
    }

    Ok(())
}

fn ensure_root_input_exists(
    path: &Path,
    lock: &FlakeLock,
    input_name: &str,
) -> Result<(), MigrationError> {
    let root_node = lock
        .nodes
        .get(&lock.root)
        .ok_or_else(|| MigrationError::InvalidLockGraph {
            path: path.to_path_buf(),
            detail: format!("root node {} is missing", lock.root),
        })?;
    if !root_node.inputs.contains_key(input_name) {
        return Err(MigrationError::InvalidLockGraph {
            path: path.to_path_buf(),
            detail: format!("selected direct input {input_name} is absent from root.inputs"),
        });
    }
    Ok(())
}

fn allowed_changed_nodes(
    lock_path: &Path,
    lock: &FlakeLock,
    input_name: &str,
) -> Result<BTreeSet<String>, MigrationError> {
    let root_node = lock
        .nodes
        .get(&lock.root)
        .ok_or_else(|| MigrationError::InvalidLockGraph {
            path: lock_path.to_path_buf(),
            detail: format!("root node {} is missing", lock.root),
        })?;
    let target_ref =
        root_node
            .inputs
            .get(input_name)
            .ok_or_else(|| MigrationError::InvalidLockGraph {
                path: lock_path.to_path_buf(),
                detail: format!("selected direct input {input_name} is absent from root.inputs"),
            })?;
    let target_node = target_ref
        .as_str()
        .ok_or_else(|| MigrationError::InvalidLockGraph {
            path: lock_path.to_path_buf(),
            detail: format!("selected direct input {input_name} does not point directly to a node"),
        })?;

    let mut allowed = BTreeSet::from([target_node.to_owned()]);
    let mut stack = vec![target_node.to_owned()];
    while let Some(node_id) = stack.pop() {
        if let Some(node) = lock.nodes.get(&node_id) {
            for input_ref in node.inputs.values() {
                if let Some(target) = input_ref.as_str() {
                    if allowed.insert(target.to_owned()) {
                        stack.push(target.to_owned());
                    }
                }
            }
        }
    }
    Ok(allowed)
}

fn changed_lock_nodes(before: &FlakeLock, after: &FlakeLock) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    for key in before
        .nodes
        .keys()
        .chain(after.nodes.keys())
        .collect::<BTreeSet<_>>()
    {
        if before.nodes.get(key) != after.nodes.get(key) {
            changed.insert(key.to_owned());
        }
    }
    if before.root != after.root {
        changed.insert("root".to_owned());
    }
    changed
}

fn resolve_input_reference(
    lock: &FlakeLock,
    starting_node_id: &str,
    input_ref: &Value,
) -> Result<String, String> {
    if let Some(target) = input_ref.as_str() {
        if lock.nodes.contains_key(target) {
            return Ok(target.to_owned());
        }
        return Err(format!("target node {target} is missing"));
    }

    let path = input_ref
        .as_array()
        .ok_or_else(|| "input reference must be a node id string or follows path".to_owned())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "follows path segments must be strings".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if path.is_empty() {
        return Err("follows path must not be empty".to_owned());
    }

    let mut current_node_id = lock.root.clone();
    for segment in path {
        let node = lock
            .nodes
            .get(&current_node_id)
            .ok_or_else(|| format!("node {current_node_id} is missing"))?;
        let next_ref = node.inputs.get(&segment).ok_or_else(|| {
            format!(
                "follows segment {segment} is missing on node {current_node_id} while resolving from {starting_node_id}"
            )
        })?;
        current_node_id = if let Some(target) = next_ref.as_str() {
            if lock.nodes.contains_key(target) {
                target.to_owned()
            } else {
                return Err(format!("target node {target} is missing"));
            }
        } else {
            resolve_input_reference(lock, starting_node_id, next_ref)?
        };
    }

    Ok(current_node_id)
}

fn ensure_clean_worktree(path: &Path) -> Result<(), MigrationError> {
    let args = vec![
        "-C".to_owned(),
        path.display().to_string(),
        "rev-parse".to_owned(),
        "--show-toplevel".to_owned(),
    ];
    let output = run_command("git", &args)?;
    let worktree_root = PathBuf::from(output.trim());
    let args = vec![
        "-C".to_owned(),
        worktree_root.display().to_string(),
        "status".to_owned(),
        "--porcelain".to_owned(),
    ];
    let status = run_command("git", &args)?;
    if !status.trim().is_empty() {
        return Err(MigrationError::DirtyWorktree { worktree_root });
    }
    Ok(())
}

fn read_nix_version() -> Result<String, MigrationError> {
    let nix_bin = nix_binary();
    Ok(run_command(
        nix_bin.to_string_lossy().as_ref(),
        &["--version".to_owned()],
    )?
    .trim()
    .to_owned())
}

fn run_nix_flake_update(flake_dir: &Path, input_name: &str) -> Result<(), MigrationError> {
    let nix_bin = nix_binary();
    let args = vec![
        "flake".to_owned(),
        "update".to_owned(),
        input_name.to_owned(),
    ];
    let _ = run_command_in_dir(nix_bin.to_string_lossy().as_ref(), flake_dir, &args)?;
    Ok(())
}

fn nix_binary() -> PathBuf {
    std::env::var_os("GIT_RELAY_NIX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("nix"))
}

fn render_combined_diff(
    before_flake: &str,
    after_flake: &str,
    before_lock: &str,
    after_lock: &str,
) -> Result<String, MigrationError> {
    let mut diffs = Vec::new();
    let flake_diff = render_diff(
        "flake.nix.before",
        before_flake,
        "flake.nix.after",
        after_flake,
    )?;
    if !flake_diff.trim().is_empty() {
        diffs.push(flake_diff);
    }
    let lock_diff = render_diff(
        "flake.lock.before",
        before_lock,
        "flake.lock.after",
        after_lock,
    )?;
    if !lock_diff.trim().is_empty() {
        diffs.push(lock_diff);
    }
    Ok(diffs.join("\n"))
}

fn render_diff(
    before_name: &str,
    before_contents: &str,
    after_name: &str,
    after_contents: &str,
) -> Result<String, MigrationError> {
    if before_contents == after_contents {
        return Ok(String::new());
    }

    let temp = tempfile::tempdir().map_err(|error| MigrationError::Write {
        path: std::env::temp_dir(),
        error,
    })?;
    let before_path = temp.path().join(before_name);
    let after_path = temp.path().join(after_name);
    fs::write(&before_path, before_contents).map_err(|error| MigrationError::Write {
        path: before_path.clone(),
        error,
    })?;
    fs::write(&after_path, after_contents).map_err(|error| MigrationError::Write {
        path: after_path.clone(),
        error,
    })?;

    let args = vec![
        "diff".to_owned(),
        "--no-index".to_owned(),
        "--no-ext-diff".to_owned(),
        "--text".to_owned(),
        before_path.display().to_string(),
        after_path.display().to_string(),
    ];
    let output = run_command_allow_diff("git", &args)?;
    Ok(output)
}

fn run_command(program: &str, args: &[String]) -> Result<String, MigrationError> {
    run_command_inner(program, None, args, false)
}

fn run_command_in_dir(
    program: &str,
    dir: &Path,
    args: &[String],
) -> Result<String, MigrationError> {
    run_command_inner(program, Some(dir), args, false)
}

fn run_command_allow_diff(program: &str, args: &[String]) -> Result<String, MigrationError> {
    run_command_inner(program, None, args, true)
}

fn run_command_inner(
    program: &str,
    dir: Option<&Path>,
    args: &[String],
    allow_exit_one: bool,
) -> Result<String, MigrationError> {
    let mut command = Command::new(program);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .args(args)
        .output()
        .map_err(|error| MigrationError::SpawnCommand {
            program: program.to_owned(),
            args: args.to_vec(),
            error,
        })?;
    if output.status.success() || (allow_exit_one && output.status.code() == Some(1)) {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    Err(MigrationError::Command {
        program: program.to_owned(),
        args: args.to_vec(),
        status: output.status.code(),
        detail,
    })
}

fn parse_key_value<'a>(
    value: &'a str,
    option_kind: &str,
) -> Result<(&'a str, &'a str), MigrationError> {
    value
        .split_once('=')
        .ok_or_else(|| MigrationError::InvalidPolicyOption {
            option_kind: option_kind.to_owned(),
            value: value.to_owned(),
        })
}

fn parse_transport(value: &str, option_kind: &str) -> Result<MigrationTransport, MigrationError> {
    match value {
        "git+https" => Ok(MigrationTransport::GitHttps),
        "git+ssh" => Ok(MigrationTransport::GitSsh),
        _ => Err(MigrationError::InvalidPolicyOption {
            option_kind: option_kind.to_owned(),
            value: value.to_owned(),
        }),
    }
}

fn parse_repo_class(value: &str, option_kind: &str) -> Result<MigrationRepoClass, MigrationError> {
    match value {
        "public" => Ok(MigrationRepoClass::Public),
        "private" => Ok(MigrationRepoClass::Private),
        _ => Err(MigrationError::InvalidPolicyOption {
            option_kind: option_kind.to_owned(),
            value: value.to_owned(),
        }),
    }
}

fn matches_shorthand_prefix(value: &str) -> bool {
    value.starts_with("github:") || value.starts_with("gitlab:") || value.starts_with("sourcehut:")
}

fn quote_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Debug, Clone)]
struct FlakeProject {
    flake_dir: PathBuf,
    flake_file: PathBuf,
    lock_file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectInputAssignment {
    input_name: String,
    value: String,
    value_span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShorthandKind {
    Github,
    Gitlab,
    Sourcehut,
}

impl ShorthandKind {
    fn label(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Sourcehut => "sourcehut",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ParsedShorthandQuery {
    host: Option<String>,
    ref_name: Option<String>,
    rev: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedShorthandUrl {
    kind: ShorthandKind,
    host: String,
    repo_path: String,
    ref_name: Option<String>,
    rev: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FlakeLock {
    nodes: BTreeMap<String, FlakeLockNode>,
    root: String,
    version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FlakeLockNode {
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    locked: Option<Value>,
    #[serde(default)]
    original: Option<Value>,
    #[serde(default)]
    parent: Option<Vec<String>>,
}

struct FlakeParser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    index: usize,
}

impl<'a> FlakeParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            index: 0,
        }
    }

    fn parse(&mut self) -> Result<Vec<DirectInputAssignment>, MigrationError> {
        let mut assignments = Vec::new();
        while self.index < self.bytes.len() {
            self.skip_noise()?;
            if self.index >= self.bytes.len() {
                break;
            }
            let checkpoint = self.index;
            if self.try_consume_keyword("inputs")? {
                let start_after_inputs = self.index;
                self.skip_noise()?;
                if self.consume_char('.') {
                    self.skip_noise()?;
                    let input_name = self.parse_identifier().ok_or_else(|| {
                        MigrationError::UnsupportedExpression {
                            input_name: "<unknown>".to_owned(),
                            detail: "expected direct input name after inputs.".to_owned(),
                        }
                    })?;
                    self.skip_noise()?;
                    if !self.consume_char('.') {
                        self.index = checkpoint + 1;
                        continue;
                    }
                    self.skip_noise()?;
                    if !self.try_consume_keyword("url")? {
                        self.index = checkpoint + 1;
                        continue;
                    }
                    self.skip_noise()?;
                    if !self.consume_char('=') {
                        return Err(MigrationError::UnsupportedExpression {
                            input_name,
                            detail: "expected '=' after direct input url path".to_owned(),
                        });
                    }
                    self.skip_noise()?;
                    let (value, value_span) = self.parse_string_literal(&input_name)?;
                    self.skip_noise()?;
                    if !self.consume_char(';') {
                        return Err(MigrationError::UnsupportedExpression {
                            input_name,
                            detail: "expected ';' after direct input url assignment".to_owned(),
                        });
                    }
                    assignments.push(DirectInputAssignment {
                        input_name,
                        value,
                        value_span,
                    });
                    continue;
                }
                self.index = start_after_inputs;
            }
            self.index = checkpoint + 1;
        }
        Ok(assignments)
    }

    fn skip_noise(&mut self) -> Result<(), MigrationError> {
        loop {
            while self.index < self.bytes.len() && self.bytes[self.index].is_ascii_whitespace() {
                self.index += 1;
            }
            if self.peek_str("#") {
                while self.index < self.bytes.len() && self.bytes[self.index] != b'\n' {
                    self.index += 1;
                }
                continue;
            }
            if self.peek_str("/*") {
                self.index += 2;
                while self.index + 1 < self.bytes.len() && !self.peek_str("*/") {
                    self.index += 1;
                }
                if self.index + 1 >= self.bytes.len() {
                    return Err(MigrationError::UnsupportedExpression {
                        input_name: "<unknown>".to_owned(),
                        detail: "unterminated block comment".to_owned(),
                    });
                }
                self.index += 2;
                continue;
            }
            break;
        }
        Ok(())
    }

    fn try_consume_keyword(&mut self, keyword: &str) -> Result<bool, MigrationError> {
        if !self.peek_str(keyword) {
            return Ok(false);
        }
        let end = self.index + keyword.len();
        if end < self.bytes.len() {
            let next = self.bytes[end] as char;
            if is_identifier_char(next) {
                return Ok(false);
            }
        }
        self.index = end;
        Ok(true)
    }

    fn parse_identifier(&mut self) -> Option<String> {
        let start = self.index;
        while self.index < self.bytes.len() {
            let character = self.bytes[self.index] as char;
            if !is_identifier_char(character) {
                break;
            }
            self.index += 1;
        }
        if self.index == start {
            None
        } else {
            Some(self.source[start..self.index].to_owned())
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.index < self.bytes.len() && self.bytes[self.index] as char == expected {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn parse_string_literal(&mut self, input_name: &str) -> Result<(String, Span), MigrationError> {
        if !self.consume_char('"') {
            return Err(MigrationError::UnsupportedExpression {
                input_name: input_name.to_owned(),
                detail: "direct input url must be a double-quoted literal string".to_owned(),
            });
        }
        let value_start = self.index - 1;
        let mut value = String::new();
        while self.index < self.bytes.len() {
            let character = self.bytes[self.index] as char;
            match character {
                '"' => {
                    let span = Span {
                        start: value_start,
                        end: self.index + 1,
                    };
                    self.index += 1;
                    return Ok((value, span));
                }
                '\\' => {
                    self.index += 1;
                    if self.index >= self.bytes.len() {
                        break;
                    }
                    let escaped = self.bytes[self.index] as char;
                    value.push(match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    self.index += 1;
                }
                '$' if self.index + 1 < self.bytes.len() && self.bytes[self.index + 1] == b'{' => {
                    return Err(MigrationError::UnsupportedExpression {
                        input_name: input_name.to_owned(),
                        detail: "interpolated strings are outside the supported literal grammar"
                            .to_owned(),
                    });
                }
                other => {
                    value.push(other);
                    self.index += 1;
                }
            }
        }
        Err(MigrationError::UnsupportedExpression {
            input_name: input_name.to_owned(),
            detail: "unterminated string literal".to_owned(),
        })
    }

    fn peek_str(&self, value: &str) -> bool {
        self.source[self.index..].starts_with(value)
    }
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::{
        build_rewrite_plan, ensure_supported_direct_coverage, parse_direct_input_assignments,
        parse_policy_overrides, parse_shorthand_url, render_git_url, DirectInputAssignment,
        DirectInputState, FlakeLock, FlakeLockNode, MigrationRepoClass, MigrationRequest,
        ShorthandKind, Span, SUPPORTED_NIX_VERSIONS,
    };
    use crate::config::{
        AppConfig, DeploymentProfile, FreshnessPolicy, GitOnlyCommandMode, GitService,
        ListenConfig, MigrationConfig, MigrationTransport, PathsConfig, PolicyConfig,
        PushAckPolicy, ReconcileConfig, RepositoryMode, ServiceManager, SupportedPlatform,
        TargetedRelockMode, WorkerMode,
    };
    use serde_json::json;

    fn config() -> AppConfig {
        AppConfig {
            listen: ListenConfig {
                ssh: "127.0.0.1:4222".to_owned(),
                https: None,
                enable_http_read: false,
                enable_http_write: false,
            },
            paths: PathsConfig {
                state_root: "/tmp".into(),
                repo_root: "/tmp/repos".into(),
                repo_config_root: "/tmp/repos.d".into(),
            },
            reconcile: ReconcileConfig {
                on_push: true,
                manual_enabled: true,
                periodic_enabled: false,
                worker_mode: WorkerMode::ShortLived,
                lock_timeout_ms: 5_000,
            },
            policy: PolicyConfig {
                default_repo_mode: RepositoryMode::CacheOnly,
                default_refresh: FreshnessPolicy::Ttl("60s".parse().expect("duration")),
                negative_cache_ttl: "5s".parse().expect("duration"),
                default_push_ack: PushAckPolicy::LocalCommit,
            },
            migration: MigrationConfig {
                supported_targets: vec![MigrationTransport::GitHttps, MigrationTransport::GitSsh],
                refuse_dirty_worktree: true,
                targeted_relock_mode: TargetedRelockMode::ValidatedOnly,
            },
            deployment: DeploymentProfile {
                platform: SupportedPlatform::Macos,
                service_manager: ServiceManager::Launchd,
                service_label: "dev.git-relay".to_owned(),
                git_only_command_mode: GitOnlyCommandMode::OpensshForceCommand,
                forced_command_wrapper: "/usr/local/bin/git-relay-ssh-force-command".into(),
                disable_forwarding: true,
                runtime_secret_env_file: "/tmp/runtime.env".into(),
                required_secret_keys: vec![],
                allowed_git_services: vec![GitService::GitUploadPack, GitService::GitReceivePack],
                supported_filesystems: vec!["apfs".to_owned()],
            },
            auth_profiles: BTreeMap::new(),
        }
    }

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn parses_github_ref_and_dir_shorthand() {
        let parsed = parse_shorthand_url(
            "nixpkgs",
            "github:NixOS/nixpkgs/nixos-unstable?dir=pkgs/top-level",
        )
        .expect("parse shorthand");
        assert_eq!(parsed.kind, ShorthandKind::Github);
        assert_eq!(
            render_git_url(&parsed, MigrationTransport::GitHttps),
            "git+https://github.com/NixOS/nixpkgs?dir=pkgs/top-level&ref=nixos-unstable"
        );
    }

    #[test]
    fn parses_gitlab_subgroup_with_host_query() {
        let parsed = parse_shorthand_url(
            "example",
            "gitlab:group/subgroup/project?host=gitlab.example.com&dir=subdir",
        )
        .expect("parse shorthand");
        assert_eq!(parsed.kind, ShorthandKind::Gitlab);
        assert_eq!(
            render_git_url(&parsed, MigrationTransport::GitHttps),
            "git+https://gitlab.example.com/group/subgroup/project?dir=subdir"
        );
    }

    #[test]
    fn parses_sourcehut_rev_shorthand() {
        let parsed =
            parse_shorthand_url("src", "sourcehut:~user/project?rev=abcdef").expect("parse");
        assert_eq!(
            render_git_url(&parsed, MigrationTransport::GitSsh),
            "git+ssh://git@git.sr.ht/~user/project?rev=abcdef"
        );
    }

    #[test]
    fn rejects_ambiguous_github_ref_query() {
        let error = parse_shorthand_url("nixpkgs", "github:NixOS/nixpkgs/nixos-unstable?ref=main")
            .expect_err("ambiguous");
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn parses_direct_input_assignments_and_rewrites_idempotently() {
        let flake = r#"
          {
            inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
            inputs.foo.url = "git+https://github.com/example/foo?ref=main";
          }
        "#;
        let assignments = parse_direct_input_assignments(flake).expect("assignments");
        let policy = parse_policy_overrides(&["nixpkgs=git+https".to_owned()], &[], &[], &[])
            .expect("policy");
        let request = MigrationRequest {
            flake_path: PathBuf::from("."),
            allow_dirty: false,
            policy,
        };
        let (reports, rewrites, rewritten) =
            build_rewrite_plan(&config(), &request, flake, &assignments).expect("plan");
        assert_eq!(rewrites.len(), 1);
        assert_eq!(reports[0].state, DirectInputState::PlannedRewrite);
        assert_eq!(reports[1].state, DirectInputState::AlreadyGitTransport);
        assert!(rewritten.contains("git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"));

        let second_assignments = parse_direct_input_assignments(&rewritten).expect("assignments");
        let (_reports, second_rewrites, second_rewritten) =
            build_rewrite_plan(&config(), &request, &rewritten, &second_assignments)
                .expect("second plan");
        assert!(second_rewrites.is_empty());
        assert_eq!(rewritten, second_rewritten);
    }

    #[test]
    fn policy_selection_supports_host_input_and_class() {
        let policy = parse_policy_overrides(
            &["special=git+ssh".to_owned()],
            &["github.com=git+https".to_owned()],
            &["private=git+ssh".to_owned()],
            &["private-repo=private".to_owned()],
        )
        .expect("policy");
        assert_eq!(
            policy.input_targets.get("special"),
            Some(&MigrationTransport::GitSsh)
        );
        assert_eq!(
            policy.host_targets.get("github.com"),
            Some(&MigrationTransport::GitHttps)
        );
        assert_eq!(
            policy.class_targets.get(&MigrationRepoClass::Private),
            Some(&MigrationTransport::GitSsh)
        );
        assert_eq!(
            policy.input_classes.get("private-repo"),
            Some(&MigrationRepoClass::Private)
        );
        assert!(SUPPORTED_NIX_VERSIONS
            .iter()
            .any(|value| value.contains("2.26.3")));
    }

    #[test]
    fn parses_github_basic_ssh_rewrite() {
        let parsed = parse_shorthand_url("nixpkgs", "github:NixOS/nixpkgs").expect("parse");
        assert_eq!(
            render_git_url(&parsed, MigrationTransport::GitSsh),
            "git+ssh://git@github.com/NixOS/nixpkgs"
        );
    }

    #[test]
    fn parses_gitlab_nested_groups_https_rewrite() {
        let parsed =
            parse_shorthand_url("example", "gitlab:group/subgroup/deeper/project?ref=main")
                .expect("parse shorthand");
        assert_eq!(
            render_git_url(&parsed, MigrationTransport::GitHttps),
            "git+https://gitlab.com/group/subgroup/deeper/project?ref=main"
        );
    }

    #[test]
    fn rejects_non_literal_direct_input_expression() {
        let error = parse_direct_input_assignments(
            r#"
            {
              inputs.nixpkgs.url = builtins.getEnv "NIXPKGS_URL";
            }
            "#,
        )
        .expect_err("non-literal input should fail");
        assert!(error.to_string().contains("double-quoted literal string"));
    }

    #[test]
    fn rejects_dynamic_interpolated_direct_input_expression() {
        let error = parse_direct_input_assignments(
            r#"
            {
              inputs.nixpkgs.url = "github:NixOS/${"nixpkgs"}";
            }
            "#,
        )
        .expect_err("dynamic input should fail");
        assert!(error.to_string().contains("interpolated strings"));
    }

    #[test]
    fn rejects_lock_visible_direct_shorthand_outside_supported_grammar() {
        let lock = FlakeLock {
            nodes: BTreeMap::from([
                (
                    "root".to_owned(),
                    FlakeLockNode {
                        inputs: BTreeMap::from([("nixpkgs".to_owned(), json!("nixpkgs"))]),
                        locked: None,
                        original: None,
                        parent: None,
                    },
                ),
                (
                    "nixpkgs".to_owned(),
                    FlakeLockNode {
                        inputs: BTreeMap::new(),
                        locked: Some(json!({
                            "type": "github",
                            "owner": "NixOS",
                            "repo": "nixpkgs"
                        })),
                        original: Some(json!({
                            "type": "github",
                            "owner": "NixOS",
                            "repo": "nixpkgs"
                        })),
                        parent: None,
                    },
                ),
            ]),
            root: "root".to_owned(),
            version: 7,
        };
        let assignments = vec![DirectInputAssignment {
            input_name: "other".to_owned(),
            value: "github:example/other".to_owned(),
            value_span: Span { start: 0, end: 0 },
        }];

        let error = ensure_supported_direct_coverage(Path::new("flake.lock"), &lock, &assignments)
            .expect_err("unsupported grammar should fail");
        assert!(error.to_string().contains("supported literal grammar"));
    }
}
