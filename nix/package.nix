{
  lib,
  stdenv,
  rustPlatform,
  installShellFiles,
  makeWrapper,
  openssh,
  tmux,
  lsof,
  procps,
  fish,
}: let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  binPath = lib.makeBinPath (
    [
      openssh
      tmux
      lsof
    ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [procps]
  );
in
  rustPlatform.buildRustPackage {
    pname = cargoToml.package.name;
    version = cargoToml.package.version;
    src = lib.cleanSourceWith {
      src = ./..;
      filter = path: _type: let
        name = baseNameOf path;
      in
        name
        != ".git"
        && name != "result"
        && name != "target";
    };
    cargoLock.lockFile = ../Cargo.lock;

    nativeBuildInputs = [
      installShellFiles
      makeWrapper
    ];
    nativeCheckInputs = [fish];
    preCheck = ''
      export FLEET_REQUIRE_FISH=1
    '';

    postInstall = ''
      export HOME="$(mktemp -d)"
      unset FLEET_CONFIG XDG_CONFIG_HOME || true

      installShellCompletion --cmd fleet \
        --bash <("$out/bin/fleet" completions bash) \
        --fish <("$out/bin/fleet" completions fish) \
        --zsh <("$out/bin/fleet" completions zsh)

      wrapProgram "$out/bin/fleet" --prefix PATH : ${lib.escapeShellArg binPath}
      wrapProgram "$out/bin/fleet-tunnel-runner" --prefix PATH : ${lib.escapeShellArg binPath}
    '';

    meta = {
      description = "SSH and tmux CLI for a small development fleet";
      homepage = "https://github.com/maximilianpw/fleet";
      mainProgram = "fleet";
      platforms = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
    };
  }
