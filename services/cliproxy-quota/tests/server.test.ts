import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type { AccountQuotaResult, QuotaReader } from "../src/credentials.ts";
import type { ProviderQuota, QuotaProvider } from "../src/providers.ts";
import { createQuotaHandler, isLoopbackAddress, parseServerArgs, startServer } from "../src/server.ts";
import { credentialDirectory } from "./fixtures.ts";

const codexQuota: ProviderQuota = {
	provider: "codex",
	windows: [{ label: "7d", usedPercent: 42, resetAtMs: null }],
	readyAccounts: 1,
	totalAccounts: 2,
	fetchedAtMs: 123,
};

function stubReader(answer: (provider: QuotaProvider) => ProviderQuota | null): QuotaReader & { asked: QuotaProvider[] } {
	const asked: QuotaProvider[] = [];
	return {
		asked,
		async getQuota(provider) {
			asked.push(provider);
			return answer(provider);
		},
		async getAccountQuotas() {
			return [];
		},
	};
}

describe("quota handler contract", () => {
	test("200 returns the quota JSON uncached", async () => {
		const handle = createQuotaHandler(stubReader(() => codexQuota));
		const response = await handle(new Request("http://127.0.0.1/quota/v1/codex"));
		expect(response.status).toBe(200);
		expect(response.headers.get("cache-control")).toBe("no-store");
		expect(response.headers.get("content-type")).toContain("application/json");
		expect(await response.json()).toEqual(codexQuota);
	});

	test("maps each path to its provider", async () => {
		const reader = stubReader(() => null);
		const handle = createQuotaHandler(reader);
		for (const provider of ["claude", "codex", "xai"]) {
			await handle(new Request(`http://127.0.0.1/quota/v1/${provider}?ignored=1`));
		}
		expect(reader.asked).toEqual(["claude", "codex", "xai"]);
	});

	test("404 for unknown paths, providers, and non-GET methods without consulting credentials", async () => {
		const reader = stubReader(() => codexQuota);
		const handle = createQuotaHandler(reader);
		for (const request of [
			new Request("http://127.0.0.1/"),
			new Request("http://127.0.0.1/quota/v1/kimi"),
			new Request("http://127.0.0.1/quota/v1/codex/extra"),
			new Request("http://127.0.0.1/quota/v2/codex"),
			new Request("http://127.0.0.1/quota/v1/codex", { method: "POST" }),
			new Request("http://127.0.0.1/quota/v1/codex", { method: "HEAD" }),
		]) {
			expect((await handle(request)).status).toBe(404);
		}
		expect(reader.asked).toEqual([]);
	});

	test("404 when the provider has no credential", async () => {
		const handle = createQuotaHandler(stubReader(() => null));
		expect((await handle(new Request("http://127.0.0.1/quota/v1/xai"))).status).toBe(404);
	});

	test("503 with no body when the reader fails", async () => {
		const handle = createQuotaHandler({
			async getQuota() {
				throw new Error("upstream secret detail");
			},
			async getAccountQuotas() {
				return [];
			},
		});
		const response = await handle(new Request("http://127.0.0.1/quota/v1/claude"));
		expect(response.status).toBe(503);
		expect(await response.text()).toBe("");
	});
});

describe("metrics endpoint", () => {
	const accounts: AccountQuotaResult[] = [
		{
			provider: "codex",
			account: "codex-a",
			quota: { ready: true, windows: [{ label: "7d", usedPercent: 42, resetAtMs: 1_788_454_137_000 }] },
		},
		{ provider: "claude", account: "claude-b", quota: null },
	];

	function metricsReader(answer: () => Promise<AccountQuotaResult[]>): QuotaReader {
		return {
			async getQuota() {
				return null;
			},
			getAccountQuotas: answer,
		};
	}

	test("serves per-account gauges as Prometheus text", async () => {
		const handle = createQuotaHandler(metricsReader(async () => accounts));
		const response = await handle(new Request("http://127.0.0.1/metrics"));
		expect(response.status).toBe(200);
		expect(response.headers.get("content-type")).toBe("text/plain; version=0.0.4; charset=utf-8");
		const body = await response.text();
		expect(body).toContain('cliproxy_quota_used_percent{provider="codex",account="codex-a",window="7d"} 42\n');
		expect(body).toContain('cliproxy_quota_up{provider="claude",account="claude-b"} 0\n');
	});

	test("503 with no body when credentials cannot be read", async () => {
		const handle = createQuotaHandler(
			metricsReader(async () => {
				throw new Error("EACCES /secret/path");
			}),
		);
		const response = await handle(new Request("http://127.0.0.1/metrics"));
		expect(response.status).toBe(503);
		expect(await response.text()).toBe("");
	});

	test("404 for non-GET methods", async () => {
		const handle = createQuotaHandler(metricsReader(async () => accounts));
		expect((await handle(new Request("http://127.0.0.1/metrics", { method: "POST" }))).status).toBe(404);
	});
});

describe("argument validation", () => {
	test("requires a credential directory and a loopback listener", () => {
		expect(parseServerArgs(["--credential-dir", "/srv/auth"])).toEqual({
			credentialDirectory: "/srv/auth",
			listenAddress: "127.0.0.1",
			port: 8318,
		});
		expect(parseServerArgs([])).toEqual({ error: "--credential-dir is required" });
		expect(parseServerArgs(["--credential-dir", "/a", "--listen-address", "0.0.0.0"])).toHaveProperty("error");
		expect(parseServerArgs(["--credential-dir", "/a", "--listen-address", "localhost"])).toHaveProperty("error");
		expect(parseServerArgs(["--credential-dir", "/a", "--port", "70000"])).toHaveProperty("error");
		expect(parseServerArgs(["--credential-dir", "/a", "--unknown"])).toHaveProperty("error");
		expect(parseServerArgs(["--help"])).toBe("help");
	});

	test("loopback detection", () => {
		expect(isLoopbackAddress("127.0.0.1")).toBe(true);
		expect(isLoopbackAddress("127.8.0.1")).toBe(true);
		expect(isLoopbackAddress("::1")).toBe(true);
		expect(isLoopbackAddress("10.0.0.1")).toBe(false);
		expect(isLoopbackAddress("::")).toBe(false);
	});
});

describe("listening server", () => {
	let server: ReturnType<typeof startServer>;
	let dir: Awaited<ReturnType<typeof credentialDirectory>>;

	beforeAll(async () => {
		dir = await credentialDirectory({});
		server = startServer({ credentialDirectory: dir.path, listenAddress: "127.0.0.1", port: 0 });
	});

	afterAll(async () => {
		server.stop(true);
		await dir[Symbol.asyncDispose]();
	});

	test("binds loopback and serves the contract over a real socket", async () => {
		expect(server.hostname).toBe("127.0.0.1");
		expect((await fetch(new URL("/quota/v1/codex", server.url))).status).toBe(404);
		expect((await fetch(new URL("/nope", server.url))).status).toBe(404);
	});
});
