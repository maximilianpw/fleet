# Disposable backend + ClickHouse + Nginx, with no internet or real secrets.
{
  pkgs,
  module,
  quotaModule,
}:
pkgs.testers.runNixOSTest {
  name = "cliproxyapi-clickhouse";
  nodes.machine = {
    config,
    lib,
    ...
  }: {
    imports = [module quotaModule];
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
    users.users.alice.isNormalUser = true;
    services.cliproxyapi = {
      enable = true;
      user = "alice";
      workingDirectory = "/home/alice";
      credentialDirectory = "/home/alice/.cli-proxy-api";
      configFile = "/run/proxy/config.yaml";
      environmentFile = "/run/proxy/backend.env";
      extraArgs = ["--local-model"];
      localClickHouse = {
        enable = true;
        environmentFile = "/run/proxy/clickhouse.env";
      };
    };
    services.clickhouse.serverConfig = {
      # Keep the disposable two-core VM small; do not tune production hosts.
      background_schedule_pool_size = 16;
      background_buffer_flush_schedule_pool_size = 4;
      background_message_broker_schedule_pool_size = 4;
      background_distributed_schedule_pool_size = 4;
    };
    # Start only after the test prepares the pre-existing runtime files.
    systemd.services.cliproxyapi.wantedBy = lib.mkForce [];
    systemd.services.clickhouse.wantedBy = lib.mkForce [];
    services.cliproxyapi-quota = {
      enable = true;
      user = "alice";
      credentialDirectory = "/home/alice/.cli-proxy-api";
    };
    services.nginx = {
      enable = true;
      virtualHosts.localhost = {
        listen = [
          {
            addr = "127.0.0.1";
            port = 8320;
          }
        ];
        locations."= /management.html".alias = "${config.services.cliproxyapi.uiPackage}/share/cliproxy-ui/management.html";
        locations."/v8/management/".proxyPass = "http://127.0.0.1:8317";
      };
    };
    environment.systemPackages = [pkgs.curl pkgs.jq];
  };

  testScript = ''
    import json
    import shlex

    machine.start()
    machine.wait_for_unit("multi-user.target")

    def write(path, text):
        machine.succeed(f"printf %s {shlex.quote(text)} > {shlex.quote(path)}")

    base = "http://127.0.0.1:8320/v8/management"
    auth = "-H 'Authorization: Bearer fixture-management'"

    def api(path):
        return json.loads(machine.succeed(f"curl -fsS {auth} '{base}{path}'"))

    def sql(query, user="prox_quota", password="fixture-runtime"):
        return machine.succeed(
            f"CLICKHOUSE_PASSWORD={password} clickhouse-client --host 127.0.0.1 "
            f"--config-file /run/proxy/client.xml --user {user} --database cliproxy_quota --query {shlex.quote(query)}"
        ).strip()

    machine.succeed("install -d -m 700 /run/proxy", "install -d -m 700 -o alice /home/alice/.cli-proxy-api")
    # Keep ambient CLI connection defaults out of the fixture.
    write("/run/proxy/client.xml", "<config/>")
    config_text = """config-version: 8
    server: {host: 127.0.0.1, port: 8317}
    management: {allow-remote: true, disable-auto-update-panel: true}
    oauth: {auth-dir: /home/alice/.cli-proxy-api}
    access: {api-keys: [fixture-api]}
    """
    write("/run/proxy/config.yaml", config_text)
    write("/run/proxy/backend.env", "MANAGEMENT_PASSWORD=fixture-management\nCLICKHOUSE_QUOTA_PASSWORD=fixture-runtime\n")
    write("/run/proxy/clickhouse.env", "CLICKHOUSE_QUOTA_ADMIN_PASSWORD=fixture-admin\nCLICKHOUSE_QUOTA_PASSWORD=fixture-runtime\n")
    write("/home/alice/.cli-proxy-api/codex.json", json.dumps({"type": "codex", "access_token": "fixture-token", "refresh_token": "fixture-refresh", "custom_metadata": "preserve-me", "disabled": True}))
    machine.succeed("chown alice /run/proxy /run/proxy/config.yaml /home/alice/.cli-proxy-api/codex.json", "chmod 400 /run/proxy/*")
    machine.succeed("chmod 600 /home/alice/.cli-proxy-api/codex.json")
    original = json.loads(machine.succeed("cat /home/alice/.cli-proxy-api/codex.json"))

    with subtest("ordered startup initializes schema without moving credentials"):
        machine.succeed("systemctl start cliproxyapi")
        machine.wait_for_open_port(8317)
        machine.wait_until_succeeds(f"curl -fsS {auth} '{base}/credentials'")
        credentials = api("/credentials")["files"]
        assert len(credentials) == 1, credentials
        account = credentials[0]["auth_index"]
        assert credentials[0]["name"] == "codex.json", credentials
        # The backend atomically canonicalizes Codex JSON and tightens its mode;
        # preserve every data field and the path, not formatting or the inode.
        assert json.loads(machine.succeed("cat /home/alice/.cli-proxy-api/codex.json")) == original
        ownership = machine.succeed("stat -c '%U %a' /home/alice/.cli-proxy-api/codex.json").strip()
        assert ownership == "alice 600", ownership
        assert api("/observability/quota/history")["history"] == {}
        assert api("/observability/quota/pools")["pools"] == []
        machine.succeed("curl -fsS http://127.0.0.1:8320/management.html > /tmp/ui.html", "cmp /tmp/ui.html " + machine.succeed("systemctl show -P Environment cliproxyapi | tr ' ' '\n' | sed -n 's/^MANAGEMENT_STATIC_PATH=//p'").strip() + "/management.html")

    with subtest("management and database authentication; least-privilege runtime user"):
        assert machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' '{base}/observability/quota/pools'").strip() == "401"
        machine.fail("clickhouse-client --host 127.0.0.1 --query 'SELECT 1'")
        machine.succeed("test -s /var/lib/clickhouse/preprocessed_configs/users.xml")
        machine.fail("grep -E 'fixture-(admin|runtime)' /var/lib/clickhouse/preprocessed_configs/users.xml")
        denied = machine.succeed("CLICKHOUSE_PASSWORD=fixture-runtime clickhouse-client --config-file /run/proxy/client.xml --host 127.0.0.1 --user prox_quota --query 'CREATE DATABASE forbidden' 2>&1 || true")
        assert "ACCESS_DENIED" in denied, denied
        listeners = machine.succeed("ss -Htln '( sport = :8123 or sport = :9000 or sport = :8317 )' | awk '{print $4}'").split()
        assert set(listeners) == {"127.0.0.1:8123", "127.0.0.1:9000", "127.0.0.1:8317"}, listeners

    with subtest("canonical observations reach UI-facing API with independently expected rates"):
        now = int(machine.succeed("date +%s").strip()) * 1000 - 1000
        reset = now + 24 * 3600000
        rows = [(now - 3600000, 80), (now, 68)]
        values = ",".join(f"('codex','{account}',{at},{remaining},{reset},'weekly',168)" for at, remaining in rows)
        sql("INSERT INTO quota_samples (provider,account,at,remaining,resetAt,window,periodHours) VALUES " + values)
        pool = api("/observability/quota/pools")["pools"][0]
        assert pool["accountCount"] == 1 and pool["capacity"] == 100 and pool["remaining"] == 68, pool
        assert pool["burn"] == 12 and pool["drain"] == 12, pool
        assert pool["projection"]["available"] is True and pool["projection"]["emptyAt"] is not None, pool
        samples = api("/observability/quota/history")["history"]["codex:" + account]
        assert [s["remaining"] for s in samples] == [80, 68], samples

    with subtest("auth directory remains writable through the management API"):
        payload = json.dumps({"type": "claude", "access_token": "fixture-second", "disabled": True})
        machine.succeed(f"curl -fsS {auth} -H 'Content-Type: application/json' --data {shlex.quote(payload)} '{base}/credentials?name=second.json'")
        machine.succeed("test -f /home/alice/.cli-proxy-api/second.json")
        assert len(api("/credentials")["files"]) == 2

    with subtest("a database outage leaves the proxy alive and recovers without a proxy restart"):
        pid = machine.succeed("systemctl show -P MainPID cliproxyapi")
        machine.succeed("systemctl stop clickhouse", "systemctl is-active cliproxyapi")
        assert machine.succeed(f"curl -s {auth} -o /dev/null -w '%{{http_code}}' '{base}/observability/quota/pools'").strip() == "502"
        assert len(api("/credentials")["files"]) == 2
        machine.succeed("systemctl start clickhouse")
        assert api("/observability/quota/pools")["pools"][0]["remaining"] == 68
        assert machine.succeed("systemctl show -P MainPID cliproxyapi") == pid

    with subtest("restart preserves schema, observations, identities, and original auth data"):
        machine.succeed("systemctl restart clickhouse", "systemctl restart cliproxyapi-quota-schema", "systemctl restart cliproxyapi")
        machine.wait_until_succeeds(f"curl -fsS {auth} '{base}/observability/quota/pools'")
        assert sql("SELECT count() FROM quota_samples FINAL") == "2"
        assert api("/observability/quota/pools")["pools"][0]["remaining"] == 68
        assert any(c["auth_index"] == account for c in api("/credentials")["files"])
        assert json.loads(machine.succeed("cat /home/alice/.cli-proxy-api/codex.json")) == original
        machine.wait_for_unit("cliproxyapi-quota.service")
        assert machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8318/quota/v1/xai").strip() == "404"

    with subtest("a mismatched auth path fails closed without creating a replacement"):
        machine.succeed("systemctl stop cliproxyapi", "chmod 600 /run/proxy/config.yaml")
        write("/run/proxy/config.yaml", config_text.replace("/home/alice/.cli-proxy-api", "/home/alice/wrong-auth"))
        machine.fail("systemctl start cliproxyapi")
        machine.succeed("test ! -e /home/alice/wrong-auth", "systemctl stop cliproxyapi")
  '';
}
