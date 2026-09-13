{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.fleet;
  inherit (lib) concatMapStrings concatMapStringsSep concatStringsSep literalExpression mkEnableOption mkIf mkMerge mkOption optionalString types;

  hostNamePattern = "[A-Za-z0-9][A-Za-z0-9._-]*";
  sshTargetPattern = "[^-[:cntrl:]][^[:cntrl:]]*";
  remoteHostPattern = "[A-Za-z0-9._-]+";
  labelPattern = "[A-Za-z0-9][A-Za-z0-9._-]*";
  portType = types.ints.between 1 65535;

  escapeTOMLString = value:
    "\""
    + lib.replaceStrings
    ["\\" "\"" "\n" "\t" "\r"]
    ["\\\\" "\\\"" "\\n" "\\t" "\\r"]
    value
    + "\"";

  isBareKey = key: builtins.match "[A-Za-z0-9_-]+" key != null;

  formatKey = key:
    if isBareKey key
    then key
    else escapeTOMLString key;

  toTOMLValue = value:
    if builtins.isBool value
    then
      if value
      then "true"
      else "false"
    else if builtins.isInt value
    then toString value
    else if builtins.isString value
    then escapeTOMLString value
    else if builtins.isList value
    then
      if value == []
      then "[]"
      else "[ ${concatMapStringsSep ", " toTOMLValue value} ]"
    else throw "programs.fleet: unsupported TOML value ${builtins.typeOf value}";

  optionalLine = name: value:
    optionalString (value != null) "${name} = ${toTOMLValue value}\n";

  renderTableHeader = path: "[${concatMapStringsSep "." formatKey path}]";

  renderAliasTarget = hostName: alias: target: ''
    ${renderTableHeader ["hosts" hostName "alias_targets" alias]}
    ${optionalLine "ssh_target" target.ssh_target}${optionalLine "tmux_target" target.tmux_target}${optionalLine "forward_target" target.forward_target}
  '';

  renderHost = name: host: let
    aliasNames = lib.sort (a: b: a < b) (lib.attrNames host.alias_targets);
  in ''
    ${renderTableHeader ["hosts" name]}
    ssh_target = ${toTOMLValue host.ssh_target}
    ${optionalLine "display_target" host.display_target}${optionalLine "tmux_target" host.tmux_target}${optionalLine "forward_target" host.forward_target}aliases = ${toTOMLValue host.aliases}
    os = ${toTOMLValue host.os}
    role = ${toTOMLValue host.role}
    user = ${toTOMLValue host.user}
    client_enrolled = ${toTOMLValue host.client_enrolled}
    gui = ${toTOMLValue host.gui}
    long_running_agents = ${toTOMLValue host.long_running_agents}
    ${optionalLine "tmux_command" host.tmux_command}${optionalLine "tmux_session" host.tmux_session}${optionalLine "t3code_port" host.t3code_port}${concatMapStrings (alias: renderAliasTarget name alias host.alias_targets.${alias}) aliasNames}
  '';

  renderMapping = mapping: ''
    [[tunnels.mappings]]
    host = ${toTOMLValue mapping.host}
    local_port = ${toTOMLValue mapping.local_port}
    remote_port = ${toTOMLValue mapping.remote_port}
    remote_host = ${toTOMLValue mapping.remote_host}
    label = ${toTOMLValue mapping.label}
  '';

  renderSettings = settings: let
    hostNames = lib.sort (a: b: a < b) (lib.attrNames settings.hosts);
    mappings = settings.tunnels.mappings;
  in ''
    schema_version = ${toTOMLValue settings.schema_version}
    current_host = ${toTOMLValue settings.current_host}

    ${concatMapStrings (name: renderHost name settings.hosts.${name}) hostNames}[tunnels]
    supervisor = ${toTOMLValue settings.tunnels.supervisor}
    ${
      if mappings == []
      then "mappings = []\n"
      else "\n" + concatMapStrings renderMapping mappings
    }
  '';

  aliasTargetModule = {
    options = {
      ssh_target = mkOption {
        type = types.nullOr (types.strMatching sshTargetPattern);
        default = null;
        description = "SSH destination override for this alias. Inherits the canonical host when null.";
      };
      tmux_target = mkOption {
        type = types.nullOr (types.strMatching sshTargetPattern);
        default = null;
        description = "SSH destination used for the default-session tmux path. Inherits the canonical host's tmux_target when null.";
      };
      forward_target = mkOption {
        type = types.nullOr (types.strMatching sshTargetPattern);
        default = null;
        description = "SSH destination used for ad-hoc and managed forwards. Inherits the canonical host when null.";
      };
    };
  };

  hostModule = {
    options = {
      ssh_target = mkOption {
        type = types.strMatching sshTargetPattern;
        description = "OpenSSH destination or alias. Display-only user metadata is not interpolated here.";
      };
      display_target = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Inventory hostname shown by list. Defaults at runtime to ssh_target.";
      };
      tmux_target = mkOption {
        type = types.nullOr (types.strMatching sshTargetPattern);
        default = null;
        description = "SSH destination for the no-session tmux path. When null, Fleet runs an explicit remote tmux command through ssh_target.";
      };
      forward_target = mkOption {
        type = types.nullOr (types.strMatching sshTargetPattern);
        default = null;
        description = "SSH destination for ad-hoc and managed forwards. Defaults at runtime to ssh_target.";
      };
      aliases = mkOption {
        type = types.listOf (types.strMatching hostNamePattern);
        description = "Alternate names for this host. Must be unique across the hosts table.";
      };
      os = mkOption {
        type = types.str;
        description = "Free-form OS metadata. Unknown strings are allowed.";
      };
      role = mkOption {
        type = types.str;
        description = "Free-form role shown by list.";
      };
      user = mkOption {
        type = types.str;
        description = "Display metadata for the remote account. Not an SSH user override.";
      };
      client_enrolled = mkOption {
        type = types.bool;
        description = "Whether an outbound Fleet identity is enrolled. list renders this as yes or no.";
      };
      gui = mkOption {
        type = types.bool;
        description = "Whether the host has a GUI or screenshot surface.";
      };
      long_running_agents = mkOption {
        type = types.bool;
        description = "Whether unattended or long-running agent work should run here.";
      };
      tmux_command = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Remote tmux executable. Defaults at runtime to tmux.";
      };
      tmux_session = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Default remote tmux session. Defaults at runtime to main.";
      };
      t3code_port = mkOption {
        type = types.nullOr portType;
        default = null;
        description = "Declared T3 Code port used by fleet t3.";
      };
      alias_targets = mkOption {
        type = types.attrsOf (types.submodule aliasTargetModule);
        default = {};
        description = "Per-alias ssh_target, tmux_target, and forward_target overrides. Keys must be declared aliases.";
      };
    };
  };

  mappingModule = {config, ...}: {
    options = {
      host = mkOption {
        type = types.strMatching hostNamePattern;
        description = "Remote Fleet host or alias. Must not be the current host.";
      };
      local_port = mkOption {
        type = portType;
        description = "Local IPv4 loopback port bound on this machine.";
      };
      remote_port = mkOption {
        type = portType;
        description = "Port on the remote host to forward to.";
      };
      remote_host = mkOption {
        type = types.strMatching remoteHostPattern;
        default = "localhost";
        description = "Remote SSH -L target. localhost covers IPv4 and IPv6 loopback. IPv6 literals are not parsed here.";
      };
      label = mkOption {
        type = types.strMatching labelPattern;
        default = "org.nix-community.home.fleet-tunnel-${toString config.local_port}";
        defaultText = literalExpression ''"org.nix-community.home.fleet-tunnel-''${toString local_port}"'';
        description = "launchd job label. Pause state is stored against this label, outside the plist.";
      };
    };
  };

  settingsModule = {
    options = {
      schema_version = mkOption {
        type = types.ints.positive;
        default = 1;
        description = "Runtime schema version. Only 1 is accepted.";
      };
      current_host = mkOption {
        type = types.strMatching hostNamePattern;
        description = "Canonical hosts-table key for this machine. Not inferred from hostname.";
      };
      hosts = mkOption {
        type = types.attrsOf (types.submodule hostModule);
        description = "Canonical host records keyed by inventory name.";
      };
      tunnels = mkOption {
        type = types.submodule {
          options = {
            supervisor = mkOption {
              type = types.enum ["none" "launchd"];
              default = "none";
              description = "Tunnel supervisor. launchd jobs are installed only on Darwin.";
            };
            mappings = mkOption {
              type = types.listOf (types.submodule mappingModule);
              default = [];
              description = "Managed localhost forwards declared for this machine.";
            };
          };
        };
        default = {
          supervisor = "none";
          mappings = [];
        };
        description = "Managed tunnel supervisor and mappings.";
      };
    };
  };

  settings = cfg.settings;
  hostNames = lib.attrNames settings.hosts;
  currentHost = settings.hosts.${settings.current_host} or null;
  allTokens = lib.concatLists (lib.mapAttrsToList (name: host: [name] ++ host.aliases) settings.hosts);
  duplicateTokens = lib.filter (token: lib.count (candidate: candidate == token) allTokens > 1) (lib.unique allTokens);
  currentTokens =
    if currentHost == null
    then []
    else [settings.current_host] ++ currentHost.aliases;
  mappingPorts = map (mapping: mapping.local_port) settings.tunnels.mappings;
  hostNameOk = name: builtins.match hostNamePattern name != null;
  badHostKeys = lib.filter (name: !hostNameOk name) hostNames;
  aliasTargetErrors = lib.concatLists (lib.mapAttrsToList (
      name: host:
        map (alias: "hosts.${name}.alias_targets.${alias} is not declared in aliases")
        (lib.filter (alias: !(lib.elem alias host.aliases)) (lib.attrNames host.alias_targets))
    )
    settings.hosts);
  unknownMappingHosts = lib.filter (mapping: !(lib.elem mapping.host allTokens)) settings.tunnels.mappings;
  localMappingHosts = lib.filter (mapping: lib.elem mapping.host currentTokens) settings.tunnels.mappings;

  lookupHost = token: let
    matches = lib.filterAttrs (name: host: name == token || lib.elem token host.aliases) settings.hosts;
    names = lib.attrNames matches;
  in
    if lib.length names == 1
    then {
      name = lib.head names;
      host = matches.${lib.head names};
    }
    else null;

  forwardTargetFor = token: let
    found = lookupHost token;
    host = found.host;
    isAlias = found.name != token;
    override =
      if isAlias && host.alias_targets ? ${token}
      then host.alias_targets.${token}.forward_target
      else null;
  in
    if found == null
    then throw "programs.fleet: mapping host ${token} does not resolve"
    else if override != null
    then override
    else if host.forward_target != null
    then host.forward_target
    else host.ssh_target;

  tunnelArgs = mapping: [
    "${cfg.package}/bin/fleet-tunnel-runner"
    (toString mapping.local_port)
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
    "127.0.0.1:${toString mapping.local_port}:${mapping.remote_host}:${toString mapping.remote_port}"
    (forwardTargetFor mapping.host)
  ];

  launchdAgents = lib.listToAttrs (map (mapping: {
      name = "fleet-tunnel-${toString mapping.local_port}";
      value = {
        enable = true;
        config = {
          Label = mapping.label;
          ProgramArguments = tunnelArgs mapping;
          RunAtLoad = true;
          KeepAlive = true;
          ThrottleInterval = 30;
          ProcessType = "Background";
        };
      };
    })
    settings.tunnels.mappings);

  enabled = cfg.enable && cfg.package != null && settings != null;
in {
  options.programs.fleet = {
    enable = mkEnableOption "Fleet CLI, config.toml, and optional Darwin tunnel jobs";

    package = mkOption {
      type = types.nullOr types.package;
      default = null;
      description = "Package providing fleet and fleet-tunnel-runner. Required when enable is true. Set it from the Fleet flake for the same system; do not take a Darwin package as a Linux check input.";
    };

    settings = mkOption {
      type = types.nullOr (types.submodule settingsModule);
      default = null;
      description = ''
        Runtime configuration written to xdg.configFile."fleet/config.toml".
        Field names match the TOML schema. Set this when enable is true.
      '';
      example = literalExpression ''
        {
          schema_version = 1;
          current_host = "laptop";
          hosts = {
            laptop = {
              ssh_target = "laptop";
              aliases = [];
              os = "darwin";
              role = "interface";
              user = "developer";
              client_enrolled = true;
              gui = true;
              long_running_agents = false;
            };
            workbox = {
              ssh_target = "workbox";
              aliases = ["dev"];
              os = "linux";
              role = "compute";
              user = "developer";
              client_enrolled = true;
              gui = false;
              long_running_agents = true;
            };
          };
          tunnels.supervisor = "none";
        }
      '';
    };
  };

  config = mkMerge [
    {
      assertions = [
        {
          assertion = !cfg.enable || (cfg.package != null && cfg.settings != null);
          message = "programs.fleet.package and programs.fleet.settings must be set when programs.fleet.enable is true";
        }
      ];
    }
    (mkIf enabled {
      assertions = [
        {
          assertion = settings.schema_version == 1;
          message = "programs.fleet.settings.schema_version must be 1";
        }
        {
          assertion = settings.hosts ? ${settings.current_host};
          message = "programs.fleet.settings.current_host must be a canonical hosts table key";
        }
        {
          assertion = badHostKeys == [];
          message = "programs.fleet.settings.hosts keys must match ${hostNamePattern}: ${concatStringsSep ", " badHostKeys}";
        }
        {
          assertion = duplicateTokens == [];
          message = "programs.fleet host names and aliases collide: ${concatStringsSep ", " duplicateTokens}";
        }
        {
          assertion = aliasTargetErrors == [];
          message = concatStringsSep "; " aliasTargetErrors;
        }
        {
          assertion = lib.length mappingPorts == lib.length (lib.unique mappingPorts);
          message = "programs.fleet.settings.tunnels.mappings local_port values must be unique";
        }
        {
          assertion = unknownMappingHosts == [];
          message = "programs.fleet mapping host must be a declared host or alias: ${concatStringsSep ", " (map (mapping: mapping.host) unknownMappingHosts)}";
        }
        {
          assertion = localMappingHosts == [];
          message = "programs.fleet mapping host must not be the current host: ${concatStringsSep ", " (map (mapping: mapping.host) localMappingHosts)}";
        }
      ];

      home.packages = [cfg.package];

      xdg.configFile."fleet/config.toml".text = renderSettings settings;

      launchd.agents = mkIf (pkgs.stdenv.hostPlatform.isDarwin && settings.tunnels.supervisor == "launchd") launchdAgents;
    })
  ];
}
