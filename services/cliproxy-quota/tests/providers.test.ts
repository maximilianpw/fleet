import { describe, expect, test } from "bun:test";
import {
	parseClaudeQuota,
	parseCodexQuota,
	parseProviderQuota,
	parseXAIQuota,
	type ProviderQuota,
} from "../src/providers.ts";
import { codexPayload } from "./fixtures.ts";

describe("Codex", () => {
	test("orders windows by declared duration", () => {
		expect(
			parseCodexQuota({
				rate_limit: {
					allowed: true,
					limit_reached: false,
					primary_window: { used_percent: 42, limit_window_seconds: 604_800, reset_at: 1_788_454_137 },
					secondary_window: { used_percent: 17.5, limit_window_seconds: 18_000, reset_at: 1_787_878_502 },
				},
			}),
		).toEqual({
			ready: true,
			windows: [
				{ label: "5h", usedPercent: 17.5, resetAtMs: 1_787_878_502_000 },
				{ label: "7d", usedPercent: 42, resetAtMs: 1_788_454_137_000 },
			],
		});
	});

	test("accepts camelCase fields and string numbers", () => {
		expect(
			parseCodexQuota({
				rateLimit: { primaryWindow: { usedPercent: "12.5", limitWindowSeconds: "2592000", resetAt: "2026-09-01T00:00:00Z" } },
			}),
		).toEqual({
			ready: true,
			windows: [{ label: "30d", usedPercent: 12.5, resetAtMs: Date.parse("2026-09-01T00:00:00Z") }],
		});
	});

	test("exhausted, disallowed, or limit-reached is not ready", () => {
		expect(parseCodexQuota(codexPayload(100)).ready).toBe(false);
		expect(parseCodexQuota(codexPayload(5, { allowed: false })).ready).toBe(false);
		expect(parseCodexQuota(codexPayload(5, { limitReached: true })).ready).toBe(false);
	});

	test("clamps usage and labels unusual durations", () => {
		const quota = parseCodexQuota({
			rate_limit: {
				primary_window: { used_percent: 140, limit_window_seconds: 7_200 },
				secondary_window: { used_percent: -3, limit_window_seconds: 1_234 },
			},
		});
		expect(quota.windows).toEqual([
			{ label: "2h", usedPercent: 100, resetAtMs: null },
			{ label: "usage", usedPercent: 0, resetAtMs: null },
		]);
	});

	test("rejects malformed responses", () => {
		expect(() => parseCodexQuota(null)).toThrow(/must be an object/);
		expect(() => parseCodexQuota({})).toThrow(/rate_limit/);
		expect(() => parseCodexQuota({ rate_limit: { primary_window: {} } })).toThrow(/no usage windows/);
	});
});

describe("Claude", () => {
	test("parses session and weekly windows", () => {
		expect(
			parseClaudeQuota({
				five_hour: { utilization: 49, resets_at: "2026-08-27T23:19:59.813Z" },
				seven_day: { utilization: 41, resets_at: "2026-08-30T16:59:59.813Z" },
			}),
		).toEqual({
			ready: true,
			windows: [
				{ label: "5h", usedPercent: 49, resetAtMs: Date.parse("2026-08-27T23:19:59.813Z") },
				{ label: "7d", usedPercent: 41, resetAtMs: Date.parse("2026-08-30T16:59:59.813Z") },
			],
		});
	});

	test("any exhausted window makes the account unavailable", () => {
		expect(parseClaudeQuota({ five_hour: { utilization: 100 }, seven_day: { utilization: 41 } }).ready).toBe(false);
	});

	test("rejects responses without windows", () => {
		expect(() => parseClaudeQuota([])).toThrow(/must be an object/);
		expect(() => parseClaudeQuota({ five_hour: { utilization: null } })).toThrow(/no usage windows/);
	});
});

describe("xAI", () => {
	test("parses weekly billing usage", () => {
		expect(
			parseXAIQuota({
				config: {
					currentPeriod: {
						type: "USAGE_PERIOD_TYPE_WEEKLY",
						start: "2026-08-24T12:11:40.078Z",
						end: "2026-08-31T12:11:40.078Z",
					},
					creditUsagePercent: 83.25,
				},
			}),
		).toEqual({
			ready: true,
			windows: [{ label: "7d", usedPercent: 83.25, resetAtMs: Date.parse("2026-08-31T12:11:40.078Z") }],
		});
	});

	test("derives the label from the period length when the type is unknown", () => {
		const quota = parseXAIQuota({
			credit_usage_percent: 100,
			current_period: { start: "2026-08-01T00:00:00Z", end: "2026-08-31T00:00:00Z" },
		});
		expect(quota).toEqual({
			ready: false,
			windows: [{ label: "30d", usedPercent: 100, resetAtMs: Date.parse("2026-08-31T00:00:00Z") }],
		});
	});

	test("requires creditUsagePercent", () => {
		expect(() => parseXAIQuota({ config: {} })).toThrow(/creditUsagePercent/);
	});
});

describe("HTTP response validation", () => {
	const valid: ProviderQuota = {
		provider: "codex",
		windows: [{ label: "7d", usedPercent: 42, resetAtMs: null }],
		readyAccounts: 1,
		totalAccounts: 2,
		fetchedAtMs: 123,
	};

	test("accepts the server's own shape", () => {
		expect(parseProviderQuota(JSON.parse(JSON.stringify(valid)), "codex")).toEqual(valid);
	});

	test("rejects wrong provider, empty windows, bad counts, and bad windows", () => {
		for (const bad of [
			{ ...valid, provider: "claude" },
			{ ...valid, windows: [] },
			{ ...valid, readyAccounts: 3 },
			{ ...valid, readyAccounts: 0.5 },
			{ ...valid, fetchedAtMs: "123" },
			{ ...valid, windows: [{ label: "7d", usedPercent: 101, resetAtMs: null }] },
			[],
		]) {
			expect(() => parseProviderQuota(bad, "codex")).toThrow(/invalid/);
		}
	});
});
