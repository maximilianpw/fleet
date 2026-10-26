import { describe, expect, test } from "bun:test";
import { formatQuotaMetrics } from "../src/metrics.ts";

describe("quota metrics", () => {
	test("renders each family once with up, ready, usage and reset gauges", () => {
		expect(
			formatQuotaMetrics([
				{
					provider: "claude",
					account: "main",
					quota: {
						ready: false,
						windows: [
							{ label: "5h", usedPercent: 100, resetAtMs: 1_788_454_137_400 },
							{ label: "7d", usedPercent: 37.5, resetAtMs: null },
						],
					},
				},
				{ provider: "xai", account: "spare", quota: null },
			]),
		).toBe(
			[
				"# HELP cliproxy_quota_up Whether the account's last quota request succeeded.",
				"# TYPE cliproxy_quota_up gauge",
				'cliproxy_quota_up{provider="claude",account="main"} 1',
				'cliproxy_quota_up{provider="xai",account="spare"} 0',
				"# HELP cliproxy_quota_ready Whether the provider currently accepts requests for the account.",
				"# TYPE cliproxy_quota_ready gauge",
				'cliproxy_quota_ready{provider="claude",account="main"} 0',
				"# HELP cliproxy_quota_used_percent Percentage of the account's quota window already used.",
				"# TYPE cliproxy_quota_used_percent gauge",
				'cliproxy_quota_used_percent{provider="claude",account="main",window="5h"} 100',
				'cliproxy_quota_used_percent{provider="claude",account="main",window="7d"} 37.5',
				"# HELP cliproxy_quota_reset_timestamp_seconds Unix time at which the account's quota window resets.",
				"# TYPE cliproxy_quota_reset_timestamp_seconds gauge",
				'cliproxy_quota_reset_timestamp_seconds{provider="claude",account="main",window="5h"} 1788454137',
				"",
			].join("\n"),
		);
	});

	test("escapes label values and drops repeated window labels", () => {
		const body = formatQuotaMetrics([
			{
				provider: "codex",
				account: 'odd"name\\x\ny',
				quota: {
					ready: true,
					windows: [
						{ label: "usage", usedPercent: 10, resetAtMs: null },
						{ label: "usage", usedPercent: 20, resetAtMs: null },
					],
				},
			},
		]);
		expect(body).toContain('account="odd\\"name\\\\x\\ny"');
		expect(body.match(/^cliproxy_quota_used_percent/gm)).toHaveLength(1);
	});
});
