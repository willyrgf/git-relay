use serde::Serialize;

use crate::config::{RepositoryDescriptor, RepositoryLifecycle, RepositoryMode};
use crate::validator::ValidationReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositorySafetyState {
    Healthy,
    Degraded,
    Divergent,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamConvergenceState {
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpstreamStartupState {
    pub upstream_id: String,
    pub state: UpstreamConvergenceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupClassification {
    pub repo_id: String,
    pub lifecycle: RepositoryLifecycle,
    pub safety: RepositorySafetyState,
    pub write_acceptance_allowed: bool,
    pub upstreams: Vec<UpstreamStartupState>,
}

pub fn classify_startup(
    descriptor: &RepositoryDescriptor,
    validation: &ValidationReport,
) -> StartupClassification {
    let mut upstreams = descriptor
        .read_upstreams
        .iter()
        .map(|upstream| UpstreamStartupState {
            upstream_id: upstream.name.clone(),
            state: UpstreamConvergenceState::Unknown,
        })
        .collect::<Vec<_>>();
    upstreams.extend(
        descriptor
            .write_upstreams
            .iter()
            .map(|upstream| UpstreamStartupState {
                upstream_id: upstream.name.clone(),
                state: UpstreamConvergenceState::Unknown,
            }),
    );

    let safety = if descriptor.mode == RepositoryMode::Authoritative && !validation.passed() {
        RepositorySafetyState::Quarantined
    } else if descriptor.lifecycle == RepositoryLifecycle::Provisioning {
        RepositorySafetyState::Degraded
    } else if descriptor.mode == RepositoryMode::Authoritative && !upstreams.is_empty() {
        RepositorySafetyState::Degraded
    } else {
        RepositorySafetyState::Healthy
    };

    StartupClassification {
        repo_id: descriptor.repo_id.clone(),
        lifecycle: descriptor.lifecycle,
        safety,
        write_acceptance_allowed: validation.write_acceptance_allowed,
        upstreams,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{
        AuthorityModel, FreshnessPolicy, PushAckPolicy, ReadUpstream, ReconcilePolicy,
        RepositoryDescriptor, RepositoryLifecycle, RepositoryMode, TrackingRefPlacement,
        WriteUpstream,
    };
    use crate::validator::{ValidationIssue, ValidationReport, ValidationStatus};

    use super::{classify_startup, RepositorySafetyState, UpstreamConvergenceState};

    fn descriptor() -> RepositoryDescriptor {
        RepositoryDescriptor {
            repo_id: "github.com/example/repo.git".to_owned(),
            canonical_identity: "github.com/example/repo.git".to_owned(),
            repo_path: "/tmp/repo.git".into(),
            mode: RepositoryMode::Authoritative,
            lifecycle: RepositoryLifecycle::Ready,
            authority_model: AuthorityModel::RelayAuthoritative,
            tracking_refs: TrackingRefPlacement::SameRepoHidden,
            refresh: FreshnessPolicy::AuthoritativeLocal,
            push_ack: PushAckPolicy::LocalCommit,
            reconcile_policy: ReconcilePolicy::OnPushManual,
            exported_refs: vec!["refs/heads/*".to_owned(), "refs/tags/*".to_owned()],
            read_upstreams: vec![ReadUpstream {
                name: "github-read".to_owned(),
                url: "ssh://git@github.com/example/repo.git".to_owned(),
                auth_profile: "read".to_owned(),
            }],
            write_upstreams: vec![WriteUpstream {
                name: "github-write".to_owned(),
                url: "ssh://git@github.com/example/repo.git".to_owned(),
                auth_profile: "write".to_owned(),
                require_atomic: true,
            }],
        }
    }

    #[test]
    fn authoritative_startup_begins_unknown_and_degraded() {
        let report = ValidationReport {
            repo_id: "github.com/example/repo.git".to_owned(),
            status: ValidationStatus::Passed,
            write_acceptance_allowed: true,
            issues: Vec::new(),
        };

        let classification = classify_startup(&descriptor(), &report);

        assert_eq!(classification.safety, RepositorySafetyState::Degraded);
        assert!(classification.write_acceptance_allowed);
        assert_eq!(
            classification
                .upstreams
                .iter()
                .map(|upstream| upstream.state)
                .collect::<Vec<_>>(),
            vec![
                UpstreamConvergenceState::Unknown,
                UpstreamConvergenceState::Unknown
            ]
        );
    }

    #[test]
    fn invalid_authoritative_repo_is_quarantined() {
        let report = ValidationReport {
            repo_id: "github.com/example/repo.git".to_owned(),
            status: ValidationStatus::Failed,
            write_acceptance_allowed: false,
            issues: vec![ValidationIssue::new("validator", "forced failure")],
        };

        let classification = classify_startup(&descriptor(), &report);

        assert_eq!(classification.safety, RepositorySafetyState::Quarantined);
        assert!(!classification.write_acceptance_allowed);
    }
}
