{
  description = "Vox — unified communication extension for Omegon agents";

  nixConfig = {
    extra-substituters      = [ "https://styrene.cachix.org" ];
    extra-trusted-public-keys = [
      "styrene.cachix.org-1:oyGX4VS45l/HvLNQvBHJ+PjIQ23mUI+XTzL8aOCvXUg="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane, nix2container }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;

        version = "0.1.0";
        commitSha =
          if self ? shortRev then self.shortRev
          else if self ? dirtyShortRev then self.dirtyShortRev
          else "unknown";

        # Crane source filtering — Rust sources + manifest.toml + config
        src = pkgs.lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type)
            || builtins.match ".*\\.toml$" path != null
            || builtins.match ".*/deploy/.*" path != null;
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          # vox uses rustls — no OpenSSL needed
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
        };

        # Build deps first (cached separately from source changes)
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          cargoExtraArgs = "--features all";
        });

        # Full vox binary with all connectors
        vox = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--features all";
        });

        # OCI images (Linux only)
        images = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux (
          import ./nix/oci.nix {
            inherit nix2container pkgs vox version commitSha;
          }
        );
      in
      {
        packages = {
          default = vox;
          vox = vox;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          oci = images.oci;
          oci-install = images.oci-install;
        };

        checks = {
          vox-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--features all";
            cargoClippyExtraArgs = "-- --deny warnings";
          });

          vox-tests = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--features all";
          });
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            cargo-watch
            cargo-zigbuild
            just
          ];
        };
      }
    );
}
