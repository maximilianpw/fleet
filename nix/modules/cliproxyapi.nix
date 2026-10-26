{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.cliproxyapi;
  ch = cfg.localClickHouse;
  inherit (lib) mkEnableOption mkIf mkOption types;
  adminClientConfig = pkgs.writeText "cliproxyapi-clickhouse-client.xml" ''
    <config>
      <host>127.0.0.1</host>
      <user>default</user>
      <password from_env="CLICKHOUSE_QUOTA_ADMIN_PASSWORD" hide_in_preprocessed="true"/>
    </config>
  '';
in {
  options.services.cliproxyapi = {
    enable = mkEnableOption "the pinned CLIProxyAPI quota-statistics backend";
    package = mkOption {
      type = types.package;
      description = "CLIProxyAPI package including share/cliproxyapi/quota-schema.sql.";
    };
    uiPackage = mkOption {
      type = types.package;
      description = "Fleet prox UI package; also usable by an existing Nginx alias.";
    };
    user = mkOption {
      type = types.str;
      example = "alice";
      description = "Existing proxy service user. No user or credential store is created.";
    };
    configFile = mkOption {
      type = types.strMatching "/.+";
      example = "/run/secrets/rendered/cliproxyapi.conf";
      description = "Existing runtime YAML config, for example a SOPS template path (not a Nix path literal). Never copied to the store or rewritten.";
    };
    credentialDirectory = mkOption {
      type = types.strMatching "/.+";
      example = "/home/alice/.cli-proxy-api";
      description = "Existing auth-files directory. Must match the config's absolute oauth.auth-dir (or legacy auth-dir). Tokens stay here and remain writable for refresh.";
    };
    workingDirectory = mkOption {
      type = types.strMatching "/.+";
      example = "/home/alice";
      description = "Existing proxy working directory; preserve it when adopting the module.";
    };
    environmentFile = mkOption {
      type = types.nullOr (types.strMatching "/.+");
      default = null;
      description = "Runtime systemd EnvironmentFile, usually rendered by SOPS/agenix. Supply CLICKHOUSE_QUOTA_PASSWORD here, never in Nix values. External storage can also set CLICKHOUSE_QUOTA_ENDPOINT/DATABASE/USERNAME.";
    };
    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "Additional server arguments, for example --local-model for offline startup.";
    };
    localClickHouse = {
      enable = mkEnableOption "a loopback-only ClickHouse with idempotent quota schema initialization";
      environmentFile = mkOption {
        type = types.strMatching "/.+";
        example = "/run/secrets/rendered/cliproxyapi-clickhouse.env";
        description = "Root-readable runtime EnvironmentFile containing CLICKHOUSE_QUOTA_PASSWORD and CLICKHOUSE_QUOTA_ADMIN_PASSWORD. Only ClickHouse and initialization receive the admin password; the backend gets its separate environmentFile.";
      };
    };
  };

  config = mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          assertion = !ch.enable || cfg.environmentFile != null;
          message = "Local ClickHouse requires services.cliproxyapi.environmentFile with the runtime user's password.";
        }
      ];

      systemd.services.cliproxyapi = {
        description = "CLIProxyAPI with ClickHouse quota statistics";
        wantedBy = ["multi-user.target"];
        wants = ["network-online.target"];
        after = ["network-online.target"] ++ lib.optionals ch.enable ["cliproxyapi-quota-schema.service"];
        requires = lib.optionals ch.enable ["cliproxyapi-quota-schema.service"];
        environment =
          {
            MANAGEMENT_STATIC_PATH = "${cfg.uiPackage}/share/cliproxy-ui";
            WRITABLE_PATH = "/var/lib/cliproxyapi";
          }
          // lib.optionalAttrs ch.enable {
            CLICKHOUSE_QUOTA_ENDPOINT = "http://127.0.0.1:8123";
            CLICKHOUSE_QUOTA_DATABASE = "cliproxy_quota";
            CLICKHOUSE_QUOTA_USERNAME = "prox_quota";
          };
        # Fail closed rather than silently starting with a new/empty auth store.
        preStart = ''
          test -d ${lib.escapeShellArg cfg.credentialDirectory}
          actual=$(${lib.getExe pkgs.yq-go} -r '.oauth.auth-dir // .auth-dir' ${lib.escapeShellArg cfg.configFile})
          if [ "$actual" != ${lib.escapeShellArg cfg.credentialDirectory} ]; then
            echo "CLIProxyAPI auth-dir does not match credentialDirectory" >&2
            exit 1
          fi
        '';
        unitConfig = {
          StartLimitIntervalSec = 300;
          StartLimitBurst = 5;
        };
        serviceConfig = {
          User = cfg.user;
          WorkingDirectory = cfg.workingDirectory;
          ExecStart = lib.escapeShellArgs ([(lib.getExe cfg.package) "--config" cfg.configFile] ++ cfg.extraArgs);
          EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;
          StateDirectory = "cliproxyapi";
          StateDirectoryMode = "0700";
          Restart = "always";
          RestartSec = 5;
          UMask = "0077";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ProtectHome = "read-only";
          ReadWritePaths = [cfg.credentialDirectory];
        };
      };
    }
    (mkIf ch.enable {
      services.clickhouse = {
        enable = true;
        serverConfig = {
          listen_host = "127.0.0.1";
          http_port = 8123;
          tcp_port = 9000;
        };
        # Passwords are substituted by ClickHouse at runtime, and excluded from
        # its preprocessed config. Replace the whole default user: the hidden
        # password attribute otherwise makes XML merging retain the empty one.
        # Both users are restricted to loopback.
        extraUsersConfig = ''
          <clickhouse>
            <users>
              <default replace="replace">
                <password replace="replace" from_env="CLICKHOUSE_QUOTA_ADMIN_PASSWORD" hide_in_preprocessed="true"/>
                <networks replace="replace"><ip>127.0.0.1</ip></networks>
                <profile>default</profile>
                <quota>default</quota>
                <access_management>1</access_management>
              </default>
              <prox_quota>
                <password from_env="CLICKHOUSE_QUOTA_PASSWORD" hide_in_preprocessed="true"/>
                <networks><ip>127.0.0.1</ip></networks>
                <profile>default</profile>
                <quota>default</quota>
                <grants><query>GRANT SELECT, INSERT ON cliproxy_quota.quota_samples</query></grants>
              </prox_quota>
            </users>
          </clickhouse>
        '';
      };
      systemd.services.clickhouse = {
        serviceConfig.EnvironmentFile = ch.environmentFile;
        preStart = ''
          test -n "$CLICKHOUSE_QUOTA_PASSWORD"
          test -n "$CLICKHOUSE_QUOTA_ADMIN_PASSWORD"
        '';
      };
      systemd.services.cliproxyapi-quota-schema = {
        description = "Initialize CLIProxyAPI quota statistics schema";
        # Later DB restarts must not stop the AI proxy; quota reads return 502
        # during an outage and recover when ClickHouse comes back.
        wants = ["clickhouse.service"];
        after = ["clickhouse.service"];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          User = "clickhouse";
          EnvironmentFile = ch.environmentFile;
          UMask = "0077";
        };
        script = ''
          ${config.services.clickhouse.package}/bin/clickhouse-client --config-file ${adminClientConfig} \
            --query 'CREATE DATABASE IF NOT EXISTS cliproxy_quota'
          ${config.services.clickhouse.package}/bin/clickhouse-client --config-file ${adminClientConfig} \
            --database cliproxy_quota --multiquery < ${cfg.package}/share/cliproxyapi/quota-schema.sql
        '';
      };
    })
  ]);
}
