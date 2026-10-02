{
  lib,
  stdenvNoCC,
  bun,
  cacert,
  makeWrapper,
}: let
  root = ../../services/cliproxy-quota;

  sourceOf = files:
    lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions (map (file: root + file) files);
    };

  # The service has no runtime dependencies; bun runs the TypeScript sources
  # directly. Only the check needs node_modules (TypeScript and bun types),
  # which are platform-independent, so one hash covers every system.
  # Recompute after changing bun.lock: set it to lib.fakeHash and build.
  devDependencies = stdenvNoCC.mkDerivation {
    name = "cliproxy-quota-dev-dependencies";
    src = sourceOf ["/package.json" "/bun.lock"];
    nativeBuildInputs = [bun cacert];
    impureEnvVars = lib.fetchers.proxyImpureEnvVars;

    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      export HOME="$TMPDIR/home"
      export BUN_INSTALL_CACHE_DIR="$TMPDIR/bun-cache"
      bun install --frozen-lockfile --ignore-scripts --no-progress
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mv node_modules "$out"
      runHook postInstall
    '';
    dontFixup = true;

    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = "sha256-4H0COwU5rx9HaagFtNvoyVHPwhDtWBkZuSM3L4CfZSo=";
  };
in
  stdenvNoCC.mkDerivation {
    pname = "cliproxy-quota";
    version = "1";
    src = sourceOf ["/src"];
    nativeBuildInputs = [makeWrapper];

    dontConfigure = true;
    dontBuild = true;

    # --no-install: never auto-install packages at runtime. The transpiler
    # cache is disabled so the service needs no writable cache directory.
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/share/cliproxy-quota"
      cp -R src "$out/share/cliproxy-quota/src"
      for entry in cliproxy-quota-server:server cliproxyapi-util:util; do
        makeWrapper ${lib.getExe bun} "$out/bin/''${entry%%:*}" \
          --set BUN_RUNTIME_TRANSPILER_CACHE_PATH 0 \
          --add-flags "--no-install $out/share/cliproxy-quota/src/''${entry#*:}.ts"
      done
      runHook postInstall
    '';

    passthru = {
      inherit devDependencies;
      tests.verify = stdenvNoCC.mkDerivation {
        name = "cliproxy-quota-verify";
        src = sourceOf ["/package.json" "/tsconfig.json" "/src" "/tests"];
        nativeBuildInputs = [bun];
        dontConfigure = true;
        buildPhase = ''
          runHook preBuild
          export HOME="$TMPDIR/home"
          cp -R ${devDependencies} node_modules
          bun test
          bun node_modules/typescript/bin/tsc --noEmit
          runHook postBuild
        '';
        installPhase = ''
          touch "$out"
        '';
      };
    };

    meta = {
      description = "Quota-only loopback endpoint and client for CLIProxyAPI credentials";
      mainProgram = "cliproxy-quota-server";
      platforms = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
    };
  }
