# Boots the NixOS module with a fictional user and credential directory. The
# VM has no internet, so a present credential exercises the upstream-failure
# path (503) rather than real provider APIs.
{
  pkgs,
  module,
  package,
}:
pkgs.testers.runNixOSTest {
  name = "cliproxy-quota";
  nodes.machine = {
    imports = [module];
    users.users.alice.isNormalUser = true;
    services.cliproxyapi-quota = {
      enable = true;
      inherit package;
      user = "alice";
      credentialDirectory = "/home/alice/.cli-proxy-api";
    };
    environment.systemPackages = [package pkgs.curl];
  };

  testScript = ''
    def status(path, method="GET"):
        return machine.succeed(
            f"curl -s -o /dev/null -w '%{{http_code}}' -X {method} http://127.0.0.1:8318{path}"
        ).strip()

    machine.wait_for_unit("cliproxyapi-quota.service")
    machine.wait_for_open_port(8318, "127.0.0.1")

    with subtest("listens on loopback only, as the configured user"):
        local = machine.succeed("ss -Htln 'sport = :8318' | awk '{print $4}'").split()
        assert local == ["127.0.0.1:8318"], local
        user = machine.succeed("ps -o user= -p $(systemctl show -P MainPID cliproxyapi-quota)").strip()
        assert user == "alice", user

    with subtest("contract without credentials"):
        assert status("/quota/v1/codex") == "404"
        assert status("/quota/v1/kimi") == "404"
        assert status("/quota/v1/codex", "POST") == "404"
        assert status("/") == "404"

    with subtest("credential files are re-read; unreachable upstreams give 503"):
        machine.succeed(
            "install -d -m 700 -o alice /home/alice/.cli-proxy-api",
            "echo '{\"type\":\"claude\",\"access_token\":\"fixture-token\"}' > /home/alice/.cli-proxy-api/claude.json",
            "chown alice /home/alice/.cli-proxy-api/claude.json",
        )
        assert status("/quota/v1/claude") == "503"
        assert status("/quota/v1/xai") == "404"

    with subtest("the service cannot write its home"):
        machine.succeed(
            "systemctl show -P ProtectHome cliproxyapi-quota | grep -qx read-only",
            "systemctl show -P ProtectSystem cliproxyapi-quota | grep -qx strict",
        )

    with subtest("cliproxyapi-util reads the loopback endpoint without a key"):
        report = machine.succeed(
            "su - alice -c 'CLIPROXYAPI_QUOTA_URL=http://127.0.0.1:8318/quota/v1 cliproxyapi-util quota --json'"
        )
        assert '"status": "unknown"' in report and '"status": "unavailable"' in report, report

    with subtest("restarts after a crash"):
        machine.succeed("systemctl kill -s KILL cliproxyapi-quota")
        machine.wait_until_succeeds("systemctl is-active cliproxyapi-quota")
        machine.wait_for_open_port(8318, "127.0.0.1")
  '';
}
