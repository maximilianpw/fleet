{
  description = "Fleet CLI for SSH, tmux, and managed localhost tunnels";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/21a67dc470149f337cecafbe965d8d252a390518";
    home-manager = {
      url = "github:nix-community/home-manager/b1d1b60084970f9d1e2b72662639dab6d039be71";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    home-manager,
  }: let
    inherit (nixpkgs) lib;
    systems = ["x86_64-linux" "aarch64-darwin"];
    forEachSystem = f: lib.genAttrs systems (system: f system);
    pkgsFor = system: nixpkgs.legacyPackages.${system};

    mkFleet = pkgs: pkgs.callPackage ./nix/package.nix {};

    laptopHost = {
      ssh_target = "laptop";
      aliases = [];
      os = "darwin";
      role = "interface";
      user = "developer";
      client_enrolled = true;
      gui = true;
      long_running_agents = false;
    };

    workboxHost = {
      ssh_target = "workbox";
      aliases = ["dev"];
      os = "linux";
      role = "compute";
      user = "developer";
      client_enrolled = true;
      gui = false;
      long_running_agents = true;
      tmux_command = "tmux";
      tmux_session = "main";
    };

    linuxSettings = {
      schema_version = 1;
      current_host = "workbox";
      hosts = {
        laptop = laptopHost;
        workbox = workboxHost;
      };
      tunnels = {
        supervisor = "none";
        mappings = [];
      };
    };

    darwinSettings = {
      schema_version = 1;
      current_host = "laptop";
      hosts = {
        laptop = laptopHost;
        workbox =
          workboxHost
          // {
            display_target = "workbox.example.test";
            tmux_target = "tm-workbox";
            forward_target = "fleet-forward-workbox";
            t3code_port = 51000;
            alias_targets.dev = {
              ssh_target = "dev";
              tmux_target = "tm-dev";
              forward_target = "fleet-forward-dev";
            };
          };
      };
      tunnels = {
        supervisor = "launchd";
        mappings = [
          {
            host = "workbox";
            local_port = 3000;
            remote_port = 3000;
            remote_host = "localhost";
          }
          {
            host = "dev";
            local_port = 5173;
            remote_port = 5173;
            remote_host = "localhost";
          }
        ];
      };
    };

    evalFleetHome = {
      pkgs,
      package,
      settings,
      homeDirectory,
      extraModules ? [],
    }:
      (home-manager.lib.homeManagerConfiguration {
        inherit pkgs;
        modules =
          [
            self.homeManagerModules.default
            {
              home.username = "developer";
              home.homeDirectory = homeDirectory;
              home.stateVersion = "26.05";
              programs.fleet.enable = true;
              programs.fleet.package = package;
              programs.fleet.settings = settings;
              targets.darwin.copyApps.enable = false;
              targets.darwin.linkApps.enable = false;
            }
          ]
          ++ extraModules;
      }).config;

    runnerTail = localPort: remoteHost: remotePort: sshTarget: [
      (toString localPort)
      "-o"
      "BatchMode=yes"
      "-o"
      "ConnectTimeout=10"
      "-o"
      "ExitOnForwardFailure=yes"
      "-o"
      "ForwardAgent=no"
      "-o"
      "ControlMaster=no"
      "-o"
      "ControlPath=none"
      "-o"
      "ServerAliveInterval=30"
      "-o"
      "ServerAliveCountMax=3"
      "-N"
      "-L"
      "127.0.0.1:${toString localPort}:${remoteHost}:${toString remotePort}"
      sshTarget
    ];

    mkFleetCheck = pkgs: fleet:
      pkgs.runCommand "fleet" {
        nativeBuildInputs = [fleet pkgs.rustc];
      } ''
        set -eu
        test "$(rustc --version | cut -d ' ' -f2)" = 1.95.0
        export HOME="$TMPDIR/empty-home"
        mkdir -p "$HOME"
        unset FLEET_CONFIG XDG_CONFIG_HOME || true
        fleet --help >/dev/null
        fleet --version >/dev/null
        fleet completions bash >bash.comp
        fleet completions zsh >zsh.comp
        fleet completions fish >fish.comp
        test -s bash.comp
        test -s zsh.comp
        test -s fish.comp
        test -x ${lib.getExe fleet}
        test -x ${fleet}/bin/fleet-tunnel-runner
        fleet --config ${./examples/config.toml} config validate
        touch "$out"
      '';

    mkHomeManagerCheck = pkgs: fleet: let
      placeholder =
        pkgs.runCommand "fleet-placeholder" {
          preferLocalBuild = true;
          allowSubstitutes = false;
        } ''
          mkdir -p "$out/bin"
          touch "$out/bin/fleet" "$out/bin/fleet-tunnel-runner"
          chmod +x "$out/bin/fleet" "$out/bin/fleet-tunnel-runner"
        '';
      darwinPkgs = import nixpkgs {system = "aarch64-darwin";};
      linuxConfig = evalFleetHome {
        inherit pkgs;
        package = placeholder;
        settings = linuxSettings;
        homeDirectory = "/home/developer";
      };
      darwinConfig = evalFleetHome {
        pkgs = darwinPkgs;
        package = placeholder;
        settings = darwinSettings;
        homeDirectory = "/Users/developer";
        extraModules = [
          {
            home.packages = lib.mkForce [];
          }
        ];
      };
      linuxToml = linuxConfig.xdg.configFile."fleet/config.toml".text;
      darwinToml = darwinConfig.xdg.configFile."fleet/config.toml".text;
      agent3000 = darwinConfig.launchd.agents."fleet-tunnel-3000";
      agent5173 = darwinConfig.launchd.agents."fleet-tunnel-5173";
      failSchema =
        builtins.tryEval
        (evalFleetHome {
          inherit pkgs;
          package = placeholder;
          settings = linuxSettings // {schema_version = 2;};
          homeDirectory = "/home/developer";
        });
      failUnknown =
        builtins.tryEval
        (evalFleetHome {
          inherit pkgs;
          package = placeholder;
          settings = linuxSettings // {unexpected_field = true;};
          homeDirectory = "/home/developer";
        });
      failLocalMapping =
        builtins.tryEval
        (evalFleetHome {
          inherit pkgs;
          package = placeholder;
          settings =
            darwinSettings
            // {
              current_host = "laptop";
              tunnels = {
                supervisor = "none";
                mappings = [
                  {
                    host = "laptop";
                    local_port = 3000;
                    remote_port = 3000;
                  }
                ];
              };
            };
          homeDirectory = "/home/developer";
        });
      failDuplicatePort =
        builtins.tryEval
        (evalFleetHome {
          inherit pkgs;
          package = placeholder;
          settings =
            darwinSettings
            // {
              tunnels = {
                supervisor = "launchd";
                mappings = [
                  {
                    host = "workbox";
                    local_port = 3000;
                    remote_port = 3000;
                  }
                  {
                    host = "workbox";
                    local_port = 3000;
                    remote_port = 5173;
                  }
                ];
              };
            };
          homeDirectory = "/home/developer";
        });
    in
      assert lib.elem placeholder linuxConfig.home.packages;
      assert linuxConfig.launchd.agents == {};
      assert lib.hasInfix ''current_host = "workbox"'' linuxToml;
      assert lib.hasInfix ''supervisor = "none"'' linuxToml;
      assert lib.hasInfix ''current_host = "laptop"'' darwinToml;
      assert lib.hasInfix ''supervisor = "launchd"'' darwinToml;
      assert lib.hasInfix ''display_target = "workbox.example.test"'' darwinToml;
      assert lib.hasInfix ''forward_target = "fleet-forward-dev"'' darwinToml;
      assert lib.hasInfix ''label = "org.nix-community.home.fleet-tunnel-3000"'' darwinToml;
      assert lib.attrNames darwinConfig.launchd.agents == ["fleet-tunnel-3000" "fleet-tunnel-5173"];
      assert agent3000.enable;
      assert agent3000.config.Label == "org.nix-community.home.fleet-tunnel-3000";
      assert agent3000.config.RunAtLoad == true;
      assert agent3000.config.KeepAlive == true;
      assert agent3000.config.ThrottleInterval == 30;
      assert agent3000.config.ProcessType == "Background";
      assert agent3000.config.StandardOutPath == null;
      assert agent3000.config.StandardErrorPath == null;
      assert lib.hasSuffix "/bin/fleet-tunnel-runner" (lib.head agent3000.config.ProgramArguments);
      assert lib.drop 1 agent3000.config.ProgramArguments
      == runnerTail 3000 "localhost" 3000 "fleet-forward-workbox";
      assert agent5173.config.Label == "org.nix-community.home.fleet-tunnel-5173";
      assert lib.hasSuffix "/bin/fleet-tunnel-runner" (lib.head agent5173.config.ProgramArguments);
      assert lib.drop 1 agent5173.config.ProgramArguments
      == runnerTail 5173 "localhost" 5173 "fleet-forward-dev";
      assert !failSchema.success;
      assert !failUnknown.success;
      assert !failLocalMapping.success;
      assert !failDuplicatePort.success;
        pkgs.runCommand "home-manager" {
          nativeBuildInputs = [fleet];
          inherit linuxToml darwinToml;
          example = ./examples/config.toml;
        } ''
          set -eu
          printf '%s\n' "$linuxToml" >linux.toml
          printf '%s\n' "$darwinToml" >darwin.toml
          fleet --config linux.toml config validate
          fleet --config darwin.toml config validate
          fleet --config "$example" config validate
          printf 'schema_version = 99\ncurrent_host = "laptop"\n[hosts]\n' >bad.toml
          set +e
          fleet --config bad.toml config validate
          status=$?
          set -e
          test "$status" -eq 2
          touch "$out"
        '';
  in {
    packages = forEachSystem (system: rec {
      fleet = mkFleet (pkgsFor system);
      default = fleet;
    });

    checks = forEachSystem (
      system: let
        pkgs = pkgsFor system;
        fleet = self.packages.${system}.fleet;
      in
        {
          fleet = mkFleetCheck pkgs fleet;
        }
        // lib.optionalAttrs (system == "x86_64-linux") {
          home-manager = mkHomeManagerCheck pkgs fleet;
          alejandra =
            pkgs.runCommand "alejandra" {
              nativeBuildInputs = [pkgs.alejandra];
            } ''
              alejandra --check ${./flake.nix} ${./nix}
              touch "$out"
            '';
        }
    );

    homeManagerModules.default = import ./nix/home-manager.nix;

    formatter = forEachSystem (system: (pkgsFor system).alejandra);

    devShells = forEachSystem (system: let
      pkgs = pkgsFor system;
    in {
      default = pkgs.mkShell {
        packages =
          [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.rustfmt
            pkgs.alejandra
            pkgs.openssh
            pkgs.tmux
            pkgs.lsof
            pkgs.fish
          ]
          ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.procps];
      };
    });
  };
}
