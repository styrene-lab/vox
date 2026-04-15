{ nix2container, pkgs, vox, version, commitSha }:
let
  n2c = nix2container.packages.${pkgs.system}.nix2container;

  # Minimal root filesystem
  initDirs = pkgs.runCommand "container-init" {} ''
    mkdir -p $out/tmp $out/data/omegon/extensions/vox $out/etc
    cat > $out/etc/passwd <<'PASSWD'
    root:x:0:0:root:/:/bin/sh
    vox:x:1000:1000:vox:/data:/bin/sh
    nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin
    PASSWD
    cat > $out/etc/group <<'GROUP'
    root:x:0:
    vox:x:1000:
    nogroup:x:65534:
    GROUP
  '';

  # Extension manifest for container deployments
  manifest = pkgs.writeTextDir "data/omegon/extensions/vox/manifest.toml" ''
    [extension]
    name = "vox"
    version = "${version}"
    description = "Unified communication connector for omegon"

    [runtime]
    type = "native"
    binary = "bin/vox"

    [startup]
    ping_method = "get_tools"
    timeout_ms = 10000

    [secrets]
    required = []
    optional = [
        "VOX_DISCORD_BOT_TOKEN",
        "VOX_SLACK_BOT_TOKEN",
        "VOX_SLACK_APP_TOKEN",
        "VOX_SIGNAL_PASSWORD",
        "VOX_EMAIL_PASSWORD",
        "VOX_MATRIX_PASSWORD",
        "VOX_LXMF_IDENTITY",
    ]
  '';

  # Install script for init-container / sidecar pattern.
  # Copies vox binary + manifest into a shared OMEGON_HOME volume.
  installScript = pkgs.writeShellApplication {
    name = "vox-install";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      OMEGON_HOME="''${OMEGON_HOME:-/data/omegon}"
      EXT_DIR="$OMEGON_HOME/extensions/vox"

      mkdir -p "$EXT_DIR/bin"
      cp ${vox}/bin/vox "$EXT_DIR/bin/vox"
      chmod +x "$EXT_DIR/bin/vox"
      cp ${manifest}/data/omegon/extensions/vox/manifest.toml "$EXT_DIR/manifest.toml"

      echo "vox ${version} installed to $EXT_DIR"
    '';
  };

  # Vox extension binary symlinked into the extension dir
  voxExtDir = pkgs.runCommand "vox-ext-dir" {} ''
    mkdir -p $out/data/omegon/extensions/vox/bin
    ln -s ${vox}/bin/vox $out/data/omegon/extensions/vox/bin/vox
  '';
in
{
  # Primary image: vox as a standalone extension binary.
  # Used by omegon pods (sidecar init-container) or direct execution.
  oci = n2c.buildImage {
    name = "ghcr.io/styrene-lab/vox";
    tag = version;

    config = {
      entrypoint = [ "${vox}/bin/vox" ];
      cmd = [ "--rpc" ];
      env = [
        "VOX_CONFIG=/etc/vox/vox.toml"
      ];
      labels = {
        "org.opencontainers.image.version" = version;
        "org.opencontainers.image.revision" = commitSha;
        "org.opencontainers.image.title" = "vox";
        "org.opencontainers.image.description" = "Communication extension for Omegon — Discord, Slack, Signal, Email, LXMF";
        "org.opencontainers.image.source" = "https://github.com/styrene-lab/vox";
        "org.opencontainers.image.licenses" = "MIT";
      };
    };

    copyToRoot = [ initDirs ];

    layers = [
      # Layer 1: TLS certs + minimal runtime (changes rarely)
      (n2c.buildLayer { deps = [
        pkgs.cacert
        pkgs.coreutils
      ]; })
      # Layer 2: vox binary + extension manifest (changes on code)
      (n2c.buildLayer { deps = [ vox manifest voxExtDir ]; })
    ];
  };

  # Init-container image: installs vox into a shared OMEGON_HOME volume.
  # Used as an init container in podman pods / k8s pods.
  #
  #   podman run --rm -v omegon-home:/data/omegon vox-install
  #
  oci-install = n2c.buildImage {
    name = "ghcr.io/styrene-lab/vox-install";
    tag = version;

    config = {
      entrypoint = [ "${installScript}/bin/vox-install" ];
      env = [
        "OMEGON_HOME=/data/omegon"
      ];
      labels = {
        "org.opencontainers.image.version" = version;
        "org.opencontainers.image.revision" = commitSha;
        "org.opencontainers.image.title" = "vox-install";
        "org.opencontainers.image.description" = "Init container: installs vox extension into shared OMEGON_HOME";
        "org.opencontainers.image.source" = "https://github.com/styrene-lab/vox";
      };
    };

    copyToRoot = [ initDirs ];

    layers = [
      (n2c.buildLayer { deps = [ pkgs.coreutils installScript vox manifest ]; })
    ];
  };
}
