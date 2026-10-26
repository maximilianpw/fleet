{
  lib,
  buildGoModule,
  go_1_26,
  fetchurl,
  fetchFromGitHub,
}: let
  # The fork's go.mod requires 1.26.10; keep Fleet's nixpkgs/Rust pin intact.
  go = go_1_26.overrideAttrs {
    version = "1.26.10";
    src = fetchurl {
      url = "https://go.dev/dl/go1.26.10.src.tar.gz";
      hash = "sha256-/WPog7ovK/ypPBZGsx6+9j4rHk0rMXyXidl2po2B828=";
    };
  };
  revision = "c050425e8dcc70654bef4745ccf62eed699f39da";
in
  (buildGoModule.override {inherit go;}) {
    pname = "cliproxyapi";
    version = "0-unstable-${builtins.substring 0 7 revision}";
    src = fetchFromGitHub {
      owner = "maximilianpw";
      repo = "CLIProxyAPI";
      rev = revision;
      hash = "sha256-EstT1LdWNB4ZBwB4Ss2u1Eic1OFy9to+/mvpaeTA9kc=";
    };
    vendorHash = "sha256-yMO0DdE0pjtFxWaQXV17enV8u0KA5fIUbE8Gui/RvWU=";
    subPackages = ["cmd/server"];
    ldflags = ["-s" "-w" "-X main.Version=quota-${builtins.substring 0 7 revision}" "-X main.Commit=${revision}"];
    checkPhase = ''
      runHook preCheck
      go test ./internal/quotastats ./internal/config ./internal/api/handlers/management ./internal/api
      runHook postCheck
    '';
    postInstall = ''
      mv "$out/bin/server" "$out/bin/cli-proxy-api"
      ln -s cli-proxy-api "$out/bin/cliproxyapi"
      install -Dm644 internal/quotastats/schema.sql "$out/share/cliproxyapi/quota-schema.sql"
      install -Dm644 docs/quota-statistics.md "$out/share/doc/cliproxyapi/quota-statistics.md"
      install -Dm644 LICENSE "$out/share/licenses/cliproxyapi/LICENSE"
    '';
    meta = {
      description = "CLIProxyAPI fork with ClickHouse quota statistics";
      homepage = "https://github.com/maximilianpw/CLIProxyAPI";
      license = lib.licenses.mit;
      mainProgram = "cli-proxy-api";
      platforms = ["x86_64-linux"];
    };
  }
