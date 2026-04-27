{
  description = "Git Relay";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";

  outputs = { self, nixpkgs, flake-utils }:
    let
      exampleConfig = ./packaging/example/git-relay.example.toml;
      exampleEnv = ./packaging/example/git-relay.env.example;

      mkArtifacts = system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;

          gitRelay = pkgs.rustPlatform.buildRustPackage {
            pname = "git-relay";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeCheckInputs = [ pkgs.git ];

            meta = {
              description = "Git-first relay and cache edge";
              license = lib.licenses.mit;
              mainProgram = "git-relay";
              platforms = [
                "x86_64-linux"
                "aarch64-linux"
                "x86_64-darwin"
                "aarch64-darwin"
              ];
            };
          };

          mkSingleBinaryPackage = binaryName:
            pkgs.runCommand binaryName { } ''
              mkdir -p $out/bin
              ln -s ${gitRelay}/bin/${binaryName} $out/bin/${binaryName}
            '';

          gitRelayServiceTemplates = pkgs.runCommand "git-relay-service-templates" {
            nativeBuildInputs = [ gitRelay ];
          } ''
            mkdir -p $out/share/git-relay
            cp ${exampleConfig} $out/share/git-relay/git-relay.example.toml
            cp ${exampleEnv} $out/share/git-relay/git-relay.env.example

            ${gitRelay}/bin/git-relay deploy render-service \
              --config ${exampleConfig} \
              --format systemd \
              --binary-path ${gitRelay}/bin/git-relayd \
              > $out/share/git-relay/git-relayd.service

            ${gitRelay}/bin/git-relay deploy render-service \
              --config ${exampleConfig} \
              --format launchd \
              --binary-path ${gitRelay}/bin/git-relayd \
              > $out/share/git-relay/dev.git-relay.plist
          '';

          serviceRenderCheck = pkgs.runCommand "git-relay-service-render-check" {
            nativeBuildInputs = [ gitRelay ];
          } ''
            systemd_output="$TMPDIR/systemd.service"
            launchd_output="$TMPDIR/dev.git-relay.plist"

            ${gitRelay}/bin/git-relay deploy render-service \
              --config ${exampleConfig} \
              --format systemd \
              --binary-path ${gitRelay}/bin/git-relayd \
              > "$systemd_output"

            ${gitRelay}/bin/git-relay deploy render-service \
              --config ${exampleConfig} \
              --format launchd \
              --binary-path ${gitRelay}/bin/git-relayd \
              > "$launchd_output"

            grep -q "EnvironmentFile=/etc/git-relay/runtime.env" "$systemd_output"
            grep -q "ExecStart=${gitRelay}/bin/git-relayd serve --config ${exampleConfig}" "$systemd_output"
            grep -q "<key>Label</key>" "$launchd_output"
            grep -q "exec '${gitRelay}/bin/git-relayd' serve --config '${exampleConfig}'" "$launchd_output"

            touch $out
          '';

          mkRfcProofCheck = { checkName, testFilter }:
            pkgs.rustPlatform.buildRustPackage {
              pname = checkName;
              version = "0.1.0";
              src = self;
              cargoLock.lockFile = ./Cargo.lock;
              nativeCheckInputs = [
                pkgs.git
                pkgs.nix
                pkgs.openssh
                pkgs.python3
              ];
              doCheck = true;
              checkPhase = ''
                runHook preCheck

                export HOME="$TMPDIR/home"
                mkdir -p "$HOME"

                export GIT_RELAY_PROOF_ENABLE=1
                export GIT_RELAY_PROOF_GATE_MODE=1
                export GIT_RELAY_PROOF_BIN_GIT_RELAY=${gitRelay}/bin/git-relay
                export GIT_RELAY_PROOF_BIN_GIT_RELAYD=${gitRelay}/bin/git-relayd
                export GIT_RELAY_PROOF_BIN_GIT_RELAY_INSTALL_HOOKS=${gitRelay}/bin/git-relay-install-hooks
                export GIT_RELAY_PROOF_BIN_GIT_RELAY_SSH_FORCE_COMMAND=${gitRelay}/bin/git-relay-ssh-force-command
                export GIT_RELAY_PROOF_SSHD_BIN=${pkgs.openssh}/bin/sshd
                export GIT_RELAY_PROOF_SSH_BIN=${pkgs.openssh}/bin/ssh
                export GIT_RELAY_PROOF_SSH_KEYGEN_BIN=${pkgs.openssh}/bin/ssh-keygen
                export GIT_RELAY_PROOF_PYTHON_BIN=${pkgs.python3}/bin/python3
                export GIT_RELAY_PROOF_GIT_HTTP_BACKEND_BIN=${pkgs.git}/libexec/git-core/git-http-backend

                cargo test --locked --offline --test rfc_proof_e2e ${testFilter} -- --test-threads=1

                runHook postCheck
              '';
              installPhase = ''
                mkdir -p $out
                touch $out/passed
              '';
            };

          rfcProofFast = mkRfcProofCheck {
            checkName = "rfc-proof-e2e-fast";
            testFilter = "proof_e2e_fast_profile_contract_declared";
          };

          rfcProofFull = mkRfcProofCheck {
            checkName = "rfc-proof-e2e-full";
            testFilter = "proof_e2e_full_profile_contract_declared";
          };

          rfcProofProviderAdmission = mkRfcProofCheck {
            checkName = "rfc-proof-provider-admission";
            testFilter = "proof_e2e_provider_admission_requires_explicit_inputs";
          };

          proofTestApp = pkgs.writeShellApplication {
            name = "git-relay-proof-test";
            runtimeInputs = [
              pkgs.cargo
              pkgs.git
              pkgs.nix
              pkgs.openssh
              pkgs.python3
              pkgs.rustc
            ];
            text = ''
              set -euo pipefail

              repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
              if [[ -z "$repo_root" ]]; then
                echo "git-relay proof app must run from inside the repository checkout" >&2
                exit 1
              fi
              cd "$repo_root"

              set_proof_env() {
                export GIT_RELAY_PROOF_ENABLE=1
                export GIT_RELAY_PROOF_GATE_MODE=1
                export GIT_RELAY_PROOF_BIN_GIT_RELAY="${gitRelay}/bin/git-relay"
                export GIT_RELAY_PROOF_BIN_GIT_RELAYD="${gitRelay}/bin/git-relayd"
                export GIT_RELAY_PROOF_BIN_GIT_RELAY_INSTALL_HOOKS="${gitRelay}/bin/git-relay-install-hooks"
                export GIT_RELAY_PROOF_BIN_GIT_RELAY_SSH_FORCE_COMMAND="${gitRelay}/bin/git-relay-ssh-force-command"
                export GIT_RELAY_PROOF_SSHD_BIN="${pkgs.openssh}/bin/sshd"
                export GIT_RELAY_PROOF_SSH_BIN="${pkgs.openssh}/bin/ssh"
                export GIT_RELAY_PROOF_SSH_KEYGEN_BIN="${pkgs.openssh}/bin/ssh-keygen"
                export GIT_RELAY_PROOF_PYTHON_BIN="${pkgs.python3}/bin/python3"
                export GIT_RELAY_PROOF_GIT_HTTP_BACKEND_BIN="${pkgs.git}/libexec/git-core/git-http-backend"
              }

              run_full_gate() {
                set_proof_env

                echo "full proof gate: validating pure flake contract check"
                nix build ".#checks.${system}.rfc-proof-e2e-full"

                echo "full proof gate: running host deterministic-core suite with locked flake toolchain"
                cargo test --locked --test rfc_proof_e2e proof_e2e_full_profile_reruns_and_hashes -- --exact --test-threads=1
              }

              run_provider_admission_gate() {
                local manifest_path="''${1:-fixtures/hosted/targets.json}"
                if [[ ! -f "$manifest_path" ]]; then
                  echo "provider-admission manifest $manifest_path does not exist" >&2
                  exit 1
                fi

                local placeholder_only
                placeholder_only="$(
                  python3 - "$manifest_path" <<'PY'
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

                if [[ "$placeholder_only" == "placeholder" ]]; then
                  echo "provider-admission policy: placeholder-only manifest, running static fixture baseline (not declared-target admission evidence)"
                  nix build ".#checks.${system}.rfc-proof-provider-admission"
                  return 0
                fi

                if [[ "$placeholder_only" == "invalid" ]]; then
                  echo "provider-admission policy: $manifest_path must contain at least one placeholder or declared target" >&2
                  exit 1
                fi

                : "''${GIT_RELAY_PROOF_PROVIDER_TARGETS:?set GIT_RELAY_PROOF_PROVIDER_TARGETS to an absolute target manifest path for declared hosted targets}"
                : "''${GIT_RELAY_PROOF_PROVIDER_CREDENTIALS:?set GIT_RELAY_PROOF_PROVIDER_CREDENTIALS to an absolute credentials file path for declared hosted targets}"

                if [[ ! -f "$GIT_RELAY_PROOF_PROVIDER_TARGETS" ]]; then
                  echo "provider-admission target manifest $GIT_RELAY_PROOF_PROVIDER_TARGETS does not exist" >&2
                  exit 1
                fi

                if [[ ! -f "$GIT_RELAY_PROOF_PROVIDER_CREDENTIALS" ]]; then
                  echo "provider-admission credentials file $GIT_RELAY_PROOF_PROVIDER_CREDENTIALS does not exist" >&2
                  exit 1
                fi

                set_proof_env
                export GIT_RELAY_PROOF_PROVIDER_TARGETS
                export GIT_RELAY_PROOF_PROVIDER_CREDENTIALS

                echo "provider-admission policy: declared hosted targets detected, running explicit-input proof suite with locked flake toolchain"
                cargo test --locked --test rfc_proof_e2e proof_e2e_provider_admission_profile_runs_required_evidence_checks -- --exact --test-threads=1
              }

              usage() {
                cat <<'EOF'
Usage:
  nix run .#test                       # canonical full deterministic-core gate + provider-admission policy baseline
  nix run .#test -- provider-admission [manifest_path]
EOF
              }

              if [[ "$#" -eq 0 ]]; then
                run_full_gate
                run_provider_admission_gate fixtures/hosted/targets.json
                exit 0
              fi

              command="$1"
              case "$command" in
                provider-admission)
                  shift || true
                  if [[ "$#" -gt 1 ]]; then
                    usage >&2
                    exit 1
                  fi
                  run_provider_admission_gate "$@"
                  ;;
                help|-h|--help)
                  usage
                  ;;
                *)
                  usage >&2
                  exit 1
                  ;;
              esac
            '';
          };

          mkApp = package: binaryName: {
            type = "app";
            program = "${package}/bin/${binaryName}";
          };
        in
        rec {
          packages = {
            default = gitRelay;
            git-relay = gitRelay;
            git-relayd = mkSingleBinaryPackage "git-relayd";
            git-relay-install-hooks = mkSingleBinaryPackage "git-relay-install-hooks";
            git-relay-ssh-force-command = mkSingleBinaryPackage "git-relay-ssh-force-command";
            git-relay-service-templates = gitRelayServiceTemplates;
          };

          apps = {
            default = mkApp packages.git-relay "git-relay";
            git-relay = mkApp packages.git-relay "git-relay";
            git-relayd = mkApp packages.git-relayd "git-relayd";
            git-relay-install-hooks = mkApp packages.git-relay-install-hooks "git-relay-install-hooks";
            git-relay-ssh-force-command = mkApp packages.git-relay-ssh-force-command "git-relay-ssh-force-command";
            test = mkApp proofTestApp "git-relay-proof-test";
          };

          checks = {
            inherit (packages)
              git-relay
              git-relayd
              git-relay-install-hooks
              git-relay-ssh-force-command
              git-relay-service-templates;
            git-relay-service-render-check = serviceRenderCheck;
            "rfc-proof-e2e-fast" = rfcProofFast;
            "rfc-proof-e2e-full" = rfcProofFull;
            "rfc-proof-provider-admission" = rfcProofProviderAdmission;
          };

          devShells = {
            default = pkgs.mkShell {
              packages = with pkgs; [
                cargo
                clippy
                nix
                rust-analyzer
                rustc
                rustfmt
              ];
            };
          };
        };

    in
    flake-utils.lib.eachSystem flake-utils.lib.allSystems mkArtifacts;
}
