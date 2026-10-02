{
  lib,
  stdenvNoCC,
  bun,
  nodejs,
  cacert,
  # Build provenance shown on the System page, e.g. a Fleet short revision.
  # Supplied by the caller; never derived from Fleet's CLI tags.
  sourceRevision ? null,
}: let
  root = ../../apps/cliproxy-ui;

  lockFiles = [
    (root + "/package.json")
    (root + "/bun.lock")
  ];

  buildFiles =
    lockFiles
    ++ [
      (root + "/LICENSE")
      (root + "/index.html")
      (root + "/src")
      (root + "/tsconfig.json")
      (root + "/tsconfig.node.json")
      (root + "/vite.config.ts")
    ];

  checkFiles =
    buildFiles
    ++ [
      (root + "/eslint.config.js")
      (root + "/scripts")
      (root + "/tests")
    ];

  sourceOf = files:
    lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions files;
    };

  # bun resolves optional native bindings (rolldown, lightningcss) per
  # platform. Pinning os/cpu makes each hash independent of the builder.
  # Recompute a hash after changing bun.lock: set it to lib.fakeHash and build.
  target =
    {
      x86_64-linux = {
        os = "linux";
        cpu = "x64";
        hash = "sha256-nWSkmranFOHxG2CvZyaCfpwAFXau/Tu6SEf08K1PPxY=";
      };
      aarch64-darwin = {
        os = "darwin";
        cpu = "arm64";
        hash = "sha256-loTX7t8fuIS/T31CpvKM6AYR1KZvBf0Pc9CchvcXqbk=";
      };
    }.${
      stdenvNoCC.hostPlatform.system
    } or (throw "cliproxy-ui: unsupported system ${stdenvNoCC.hostPlatform.system}");

  # Fixed-output: the only step with network access. Keyed on the lockfile,
  # not the app version, so source edits do not refetch dependencies.
  nodeModules = stdenvNoCC.mkDerivation {
    name = "cliproxy-ui-node-modules";
    src = sourceOf lockFiles;
    nativeBuildInputs = [bun cacert];
    impureEnvVars = lib.fetchers.proxyImpureEnvVars;

    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      export HOME="$TMPDIR/home"
      export BUN_INSTALL_CACHE_DIR="$TMPDIR/bun-cache"
      bun install --frozen-lockfile --ignore-scripts --no-progress \
        --os=${target.os} --cpu=${target.cpu}
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mv node_modules "$out"
      runHook postInstall
    '';
    # Fixed-output paths may not reference the store, so shebangs are patched
    # in the consumers below.
    dontFixup = true;

    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = target.hash;
  };

  linkNodeModules = ''
    cp -R ${nodeModules} node_modules
    chmod -R u+w node_modules
    patchShebangs node_modules
  '';

  version =
    if sourceRevision == null
    then "dev"
    else sourceRevision;

  meta = {
    description = "Single-file CLIProxyAPI management UI";
    license = lib.licenses.mit;
    platforms = [
      "x86_64-linux"
      "aarch64-darwin"
    ];
  };
in
  stdenvNoCC.mkDerivation {
    pname = "cliproxy-ui";
    inherit version meta;
    src = sourceOf buildFiles;
    nativeBuildInputs = [bun nodejs];

    configurePhase = ''
      runHook preConfigure
      export HOME="$TMPDIR/home"
      ${linkNodeModules}
      runHook postConfigure
    '';

    buildPhase = ''
      runHook preBuild
      VERSION=${lib.escapeShellArg "cliproxy-ui-${version}"} bun run build
      runHook postBuild
    '';

    # The deployed artifact is one self-contained HTML file.
    installPhase = ''
      runHook preInstall
      test "$(ls -A dist)" = index.html
      if grep -Eq '<script[^>]*[[:space:]]src=|<link[^>]*rel="(stylesheet|modulepreload)"' dist/index.html; then
        echo "cliproxy-ui: dist/index.html references external scripts or styles" >&2
        exit 1
      fi
      install -Dm644 dist/index.html "$out/share/cliproxy-ui/management.html"
      install -Dm644 LICENSE "$out/share/licenses/cliproxy-ui/LICENSE"
      runHook postInstall
    '';

    passthru = {
      inherit nodeModules;
      # Unit tests and lint. Kept out of the package build so the deployable
      # artifact does not depend on test-only files.
      tests.verify = stdenvNoCC.mkDerivation {
        name = "cliproxy-ui-verify";
        src = sourceOf checkFiles;
        nativeBuildInputs = [bun nodejs];
        dontConfigure = true;
        buildPhase = ''
          runHook preBuild
          export HOME="$TMPDIR/home"
          ${linkNodeModules}
          bun test
          bun run lint
          bun run type-check
          runHook postBuild
        '';
        installPhase = ''
          touch "$out"
        '';
      };
    };
  }
