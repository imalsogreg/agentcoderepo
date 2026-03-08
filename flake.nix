{
  description = "AgentCodeRepo - agent-first code hosting";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common arguments for all crane builds
        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk_11_0.frameworks.Security
            pkgs.darwin.apple_sdk_11_0.frameworks.SystemConfiguration
          ];
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        # Build deps separately for caching
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # The server binary
        agentcoderepo-server = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--bin agentcoderepo-server";
        });

      in {
        packages = {
          default = agentcoderepo-server;

          container = pkgs.dockerTools.buildImage {
            name = "agentcoderepo";
            tag = "latest";
            copyToRoot = pkgs.buildEnv {
              name = "agentcoderepo-root";
              paths = [
                agentcoderepo-server
                pkgs.cacert
              ];
            };
            config = {
              Cmd = [ "${agentcoderepo-server}/bin/agentcoderepo-server" ];
              Env = [
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              ];
              ExposedPorts = { "8080/tcp" = {}; };
            };
          };
        };

        checks = {
          # Run clippy
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          # Run tests
          tests = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
          });

          # Check formatting
          fmt = craneLib.cargoFmt { src = commonArgs.src; };
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          packages = with pkgs; [
            rust-analyzer
            cargo-watch
            cargo-nextest
            flyctl
          ];
        };
      }
    );
}
