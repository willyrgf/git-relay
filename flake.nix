{
  description = "Git Relay";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems = f:
        builtins.listToAttrs (map (system: {
          name = system;
          value = f system;
        }) supportedSystems);

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
            testFilter = "proof_e2e_fast_profile_runs_required_cases";
          };

          rfcProofFull = mkRfcProofCheck {
            checkName = "rfc-proof-e2e-full";
            testFilter = "proof_e2e_full_profile_reruns_and_hashes";
          };

          rfcProofProviderAdmission = mkRfcProofCheck {
            checkName = "rfc-proof-provider-admission";
            testFilter = "proof_e2e_provider_admission";
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

      perSystem = forAllSystems mkArtifacts;
    in
    {
      packages = builtins.mapAttrs (_: artifacts: artifacts.packages) perSystem;
      apps = builtins.mapAttrs (_: artifacts: artifacts.apps) perSystem;
      checks = builtins.mapAttrs (_: artifacts: artifacts.checks) perSystem;
      devShells = builtins.mapAttrs (_: artifacts: artifacts.devShells) perSystem;
    };
}
