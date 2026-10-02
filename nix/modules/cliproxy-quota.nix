{
  config,
  lib,
  ...
}: let
  cfg = config.services.cliproxyapi-quota;
  inherit (lib) getExe' hasPrefix mkEnableOption mkIf mkOption types;
  isLoopback = address: hasPrefix "127." address || address == "::1";
in {
  options.services.cliproxyapi-quota = {
    enable = mkEnableOption "the CLIProxyAPI quota-only loopback endpoint";

    package = mkOption {
      type = types.package;
      description = "Package providing `cliproxy-quota-server`.";
    };

    user = mkOption {
      type = types.str;
      example = "alice";
      description = ''
        Account the service runs as. It needs read access to
        `credentialDirectory`; usually the CLIProxyAPI service user.
      '';
    };

    credentialDirectory = mkOption {
      type = types.strMatching "/.+";
      example = "/home/alice/.cli-proxy-api";
      description = ''
        CLIProxyAPI's provider credential directory. Read on demand, never
        written. Runtime data; it is not copied into the Nix store.
      '';
    };

    listenAddress = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = ''
        Loopback IP literal to bind. The endpoint has no authentication; put
        an authenticating reverse proxy in front of it for remote clients.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 8318;
      description = "TCP port for `GET /quota/v1/{claude,codex,xai}`.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = isLoopback cfg.listenAddress;
        message = "services.cliproxyapi-quota.listenAddress must be a loopback IP literal, got ${cfg.listenAddress}";
      }
    ];

    systemd.services.cliproxyapi-quota = {
      description = "CLIProxyAPI quota-only loopback endpoint";
      wantedBy = ["multi-user.target"];
      wants = ["network-online.target"];
      after = ["network-online.target"];
      serviceConfig = {
        User = cfg.user;
        ExecStart = lib.escapeShellArgs [
          (getExe' cfg.package "cliproxy-quota-server")
          "--credential-dir"
          cfg.credentialDirectory
          "--listen-address"
          cfg.listenAddress
          "--port"
          (toString cfg.port)
        ];
        Restart = "always";
        RestartSec = 5;

        # Reads credentials and calls provider HTTPS APIs; writes nothing.
        CapabilityBoundingSet = "";
        LockPersonality = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = "read-only";
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        RestrictAddressFamilies = ["AF_INET" "AF_INET6" "AF_UNIX"];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        UMask = "0077";
      };
    };
  };
}
