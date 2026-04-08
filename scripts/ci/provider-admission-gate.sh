#!/usr/bin/env bash
set -euo pipefail

manifest_path="${1:-fixtures/hosted/targets.json}"

if [[ ! -f "${manifest_path}" ]]; then
  echo "provider-admission manifest ${manifest_path} does not exist" >&2
  exit 1
fi

placeholder_only="$(
  python3 - "${manifest_path}" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    data = json.load(fh)

targets = data.get("targets")
if not isinstance(targets, list) or not targets:
    print("invalid")
    raise SystemExit(0)

def is_placeholder(target):
    target_id = str(target.get("target_id", ""))
    product = str(target.get("product", ""))
    url = str(target.get("url", ""))
    credential_source = str(target.get("credential_source", ""))
    return (
        target_id.startswith("replace-me")
        and product.startswith("replace-me")
        and "example.invalid" in url
        and credential_source.startswith("env:REPLACE_ME")
    )

print("placeholder" if all(is_placeholder(target) for target in targets) else "declared")
PY
)"

system="$(nix eval --impure --expr builtins.currentSystem --raw)"

if [[ "${placeholder_only}" == "placeholder" ]]; then
  echo "provider-admission policy: placeholder-only manifest, running flake provider-admission check"
  nix build ".#checks.${system}.rfc-proof-provider-admission"
  exit 0
fi

if [[ "${placeholder_only}" == "invalid" ]]; then
  echo "provider-admission policy: ${manifest_path} must contain at least one placeholder or declared target" >&2
  exit 1
fi

: "${GIT_RELAY_PROOF_PROVIDER_TARGETS:?set GIT_RELAY_PROOF_PROVIDER_TARGETS to an absolute target manifest path for declared hosted targets}"
: "${GIT_RELAY_PROOF_PROVIDER_CREDENTIALS:?set GIT_RELAY_PROOF_PROVIDER_CREDENTIALS to an absolute credentials file path for declared hosted targets}"

if [[ ! -f "${GIT_RELAY_PROOF_PROVIDER_TARGETS}" ]]; then
  echo "provider-admission target manifest ${GIT_RELAY_PROOF_PROVIDER_TARGETS} does not exist" >&2
  exit 1
fi

if [[ ! -f "${GIT_RELAY_PROOF_PROVIDER_CREDENTIALS}" ]]; then
  echo "provider-admission credentials file ${GIT_RELAY_PROOF_PROVIDER_CREDENTIALS} does not exist" >&2
  exit 1
fi

git_relay_out="$(nix build --no-link --print-out-paths .#git-relay)"
git_relayd_out="$(nix build --no-link --print-out-paths .#git-relayd)"
install_hooks_out="$(nix build --no-link --print-out-paths .#git-relay-install-hooks)"
ssh_force_out="$(nix build --no-link --print-out-paths .#git-relay-ssh-force-command)"
git_out="$(nix build --no-link --print-out-paths nixpkgs#git)"
openssh_out="$(nix build --no-link --print-out-paths nixpkgs#openssh)"
python_out="$(nix build --no-link --print-out-paths nixpkgs#python3)"

export GIT_RELAY_PROOF_ENABLE=1
export GIT_RELAY_PROOF_GATE_MODE=1
export GIT_RELAY_PROOF_PROVIDER_TARGETS
export GIT_RELAY_PROOF_PROVIDER_CREDENTIALS
export GIT_RELAY_PROOF_BIN_GIT_RELAY="${git_relay_out}/bin/git-relay"
export GIT_RELAY_PROOF_BIN_GIT_RELAYD="${git_relayd_out}/bin/git-relayd"
export GIT_RELAY_PROOF_BIN_GIT_RELAY_INSTALL_HOOKS="${install_hooks_out}/bin/git-relay-install-hooks"
export GIT_RELAY_PROOF_BIN_GIT_RELAY_SSH_FORCE_COMMAND="${ssh_force_out}/bin/git-relay-ssh-force-command"
export GIT_RELAY_PROOF_SSHD_BIN="${openssh_out}/bin/sshd"
export GIT_RELAY_PROOF_SSH_BIN="${openssh_out}/bin/ssh"
export GIT_RELAY_PROOF_SSH_KEYGEN_BIN="${openssh_out}/bin/ssh-keygen"
export GIT_RELAY_PROOF_PYTHON_BIN="${python_out}/bin/python3"
export GIT_RELAY_PROOF_GIT_HTTP_BACKEND_BIN="${git_out}/libexec/git-core/git-http-backend"

echo "provider-admission policy: declared hosted targets detected, running explicit-input proof suite with Nix-built binaries"
nix develop -c cargo test --locked --test rfc_proof_e2e proof_e2e_provider_admission_profile_runs_required_evidence_checks -- --exact --test-threads=1
