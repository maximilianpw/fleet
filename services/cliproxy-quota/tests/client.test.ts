import { describe, expect, test } from "bun:test";
import type { QuotaReader } from "../src/credentials.ts";
import { createQuotaClient, isLoopbackUrl, resolveQuotaConnection } from "../src/client.ts";
import type { ProviderQuota } from "../src/providers.ts";
import { formatQuotaReport, getQuotaReport } from "../src/report.ts";
import { createQuotaHandler } from "../src/server.ts";

const files = (entries: Record<string, string>) => (path: string) => {
	const body = entries[path];
	if (body === undefined) throw Object.assign(new Error(`ENOENT: ${path}`), { code: "ENOENT" });
	return body;
};

describe("connection resolution", () => {
	test("explicit quota URL from client.json with a key file", () => {
		expect(
			resolveQuotaConnection(
				{},
				{
					configFilePath: "/home/test/.config/cliproxyapi/client.json",
					readTextFile: files({
						"/home/test/.config/cliproxyapi/client.json": JSON.stringify({
							rootUrl: "https://proxy.example.test",
							quotaUrl: "https://proxy.example.test/quota/v1/",
							apiKeyFile: "/run/secrets/key",
						}),
						"/run/secrets/key": "public-key\n",
					}),
				},
			),
		).toEqual({ quotaUrl: "https://proxy.example.test/quota/v1", apiKey: "public-key" });
	});

	test("environment overrides the file", () => {
		expect(
			resolveQuotaConnection(
				{ CLIPROXYAPI_QUOTA_URL: "https://other.example.test/quota/v1", CLIPROXYAPI_API_KEY: " env-key " },
				{
					configFilePath: "/c.json",
					readTextFile: files({ "/c.json": JSON.stringify({ quotaUrl: "https://file.example.test/quota/v1" }) }),
				},
			),
		).toEqual({ quotaUrl: "https://other.example.test/quota/v1", apiKey: "env-key" });
	});

	test("a public root URL implies its /quota/v1", () => {
		expect(
			resolveQuotaConnection(
				{ CLIPROXYAPI_ROOT_URL: "https://proxy.example.test/", CLIPROXYAPI_API_KEY: "k" },
				{ configFilePath: null },
			),
		).toEqual({ quotaUrl: "https://proxy.example.test/quota/v1", apiKey: "k" });
	});

	test("a loopback root URL is the proxy, so the quota URL must be explicit", () => {
		expect(() =>
			resolveQuotaConnection({ CLIPROXYAPI_ROOT_URL: "http://127.0.0.1:8317", CLIPROXYAPI_API_KEY: "k" }, { configFilePath: null }),
		).toThrow(/set CLIPROXYAPI_QUOTA_URL/);
		expect(() => resolveQuotaConnection({}, { configFilePath: null })).toThrow(/requires CLIPROXYAPI_QUOTA_URL/);
	});

	test("loopback quota access needs no key; public access does", () => {
		expect(
			resolveQuotaConnection({ CLIPROXYAPI_QUOTA_URL: "http://127.0.0.1:8318/quota/v1" }, { configFilePath: null }),
		).toEqual({ quotaUrl: "http://127.0.0.1:8318/quota/v1", apiKey: null });
		expect(() =>
			resolveQuotaConnection({ CLIPROXYAPI_QUOTA_URL: "https://proxy.example.test/quota/v1" }, { configFilePath: null }),
		).toThrow(/requires CLIPROXYAPI_API_KEY/);
	});

	test("a missing client.json is not an error; a malformed one is", () => {
		expect(
			resolveQuotaConnection(
				{ CLIPROXYAPI_QUOTA_URL: "http://localhost:8318/quota/v1" },
				{ configFilePath: "/missing.json", readTextFile: files({}) },
			).quotaUrl,
		).toBe("http://localhost:8318/quota/v1");
		expect(() =>
			resolveQuotaConnection({}, { configFilePath: "/bad.json", readTextFile: files({ "/bad.json": "[]" }) }),
		).toThrow(/JSON object/);
	});

	test("loopback URL detection", () => {
		expect(isLoopbackUrl("http://127.0.0.1:8318/quota/v1")).toBe(true);
		expect(isLoopbackUrl("http://localhost/quota/v1")).toBe(true);
		expect(isLoopbackUrl("http://[::1]:8318/quota/v1")).toBe(true);
		expect(isLoopbackUrl("https://proxy.example.test/quota/v1")).toBe(false);
		expect(isLoopbackUrl("http://127.example.test/")).toBe(false);
	});
});

const quota: ProviderQuota = {
	provider: "codex",
	windows: [{ label: "7d", usedPercent: 42, resetAtMs: null }],
	readyAccounts: 1,
	totalAccounts: 2,
	fetchedAtMs: 123,
};

describe("HTTP client against the server handler", () => {
	const reader: QuotaReader = {
		async getQuota(provider) {
			if (provider === "codex") return quota;
			if (provider === "claude") throw new Error("upstream down");
			return null;
		},
	};
	const handle = createQuotaHandler(reader);

	test("round-trips the contract and sends the bearer token", async () => {
		const seen: (string | null)[] = [];
		const client = createQuotaClient(
			{ quotaUrl: "https://proxy.example.test/quota/v1", apiKey: "proxy-key" },
			async (input, init) => {
				seen.push(new Headers(init?.headers).get("authorization"));
				return handle(new Request(String(input)));
			},
		);
		expect(await client.getQuota("codex")).toEqual(quota);
		expect(await client.getQuota("xai")).toBeNull();
		await expect(client.getQuota("claude")).rejects.toThrow(/HTTP 503/);
		expect(seen).toEqual(["Bearer proxy-key", "Bearer proxy-key", "Bearer proxy-key"]);
	});

	test("loopback access sends no authorization header", async () => {
		const client = createQuotaClient({ quotaUrl: "http://127.0.0.1:8318/quota/v1", apiKey: null }, async (input, init) => {
			expect(new Headers(init?.headers).has("authorization")).toBe(false);
			return handle(new Request(String(input)));
		});
		expect(await client.getQuota("codex")).toEqual(quota);
	});

	test("rejects malformed replies instead of inventing quota", async () => {
		const client = createQuotaClient({ quotaUrl: "https://p.example.test/quota/v1", apiKey: "k" }, async () =>
			Response.json({ provider: "codex", windows: [] }),
		);
		await expect(client.getQuota("codex")).rejects.toThrow(/invalid/);
	});

	test("times out a hung server", async () => {
		const client = createQuotaClient(
			{ quotaUrl: "https://p.example.test/quota/v1", apiKey: "k" },
			(_input, init) =>
				new Promise((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(init.signal?.reason))),
			50,
		);
		await expect(client.getQuota("codex")).rejects.toThrow();
	});
});

describe("quota report", () => {
	test("is deterministic, distinguishes unavailable from unknown, and formats a table", async () => {
		const report = await getQuotaReport({
			async getQuota(provider) {
				if (provider === "xai") {
					return { provider, windows: [{ label: "7d", usedPercent: 100, resetAtMs: null }], readyAccounts: 0, totalAccounts: 1, fetchedAtMs: 0 };
				}
				if (provider === "claude") throw new Error("down");
				return { ...quota, windows: [{ label: "5h", usedPercent: 5, resetAtMs: null }, { label: "7d", usedPercent: 12.5, resetAtMs: null }] };
			},
		});
		expect(report).toEqual({
			version: 1,
			families: [
				{ family: "codex", status: "available", usedPercent: 12.5, readyAccounts: 1, totalAccounts: 2 },
				{ family: "claude", status: "unknown", usedPercent: null, readyAccounts: 0, totalAccounts: 0 },
				{ family: "grok", status: "unavailable", usedPercent: 100, readyAccounts: 0, totalAccounts: 1 },
			],
		});
		expect(formatQuotaReport(report)).toBe(
			[
				"family  status       usage  accounts",
				"codex   available    12.5%  1/2",
				"claude  unknown      -      -",
				"grok    unavailable  100%   0/1",
			].join("\n"),
		);
	});

	test("no credential on the server reports unavailable", async () => {
		const report = await getQuotaReport({ getQuota: async () => null });
		expect(report.families.map((family) => family.status)).toEqual(["unavailable", "unavailable", "unavailable"]);
	});
});
