import { describe, expect, test } from "bun:test";
import type { Fetch } from "../src/credentials.ts";
import { createQuotaHistoryHandler } from "../src/quotaHistory.ts";

const now = 1_800_000_000_000;
const PROMETHEUS = "http://127.0.0.1:9090";
const MANAGEMENT = "http://127.0.0.1:8317";

function matrix(metric: string, points: [number, number][]) {
	return {
		status: "success",
		data: {
			resultType: "matrix",
			result: [
				{ metric: { __name__: metric, provider: "codex", account: "codex-main", window: "7d" }, values: points.map(([at, value]) => [at / 1_000, String(value)]) },
				{ metric: { __name__: metric, provider: "codex", account: "codex-main", window: "5h" }, values: points.map(([at]) => [at / 1_000, "1"]) },
			],
		},
	};
}

function stubFetch({ management = 200, prometheus = 200 }: { management?: number; prometheus?: number } = {}) {
	const calls: { url: URL; headers: Headers }[] = [];
	const fetch: Fetch = async (input, init) => {
		const url = new URL(String(input));
		calls.push({ url, headers: new Headers(init?.headers) });
		if (url.origin === MANAGEMENT) {
			return management === 200
				? Response.json({ files: [{ name: "codex-main.json", auth_index: "7", type: "codex" }] })
				: Response.json({ error: "nope" }, { status: management });
		}
		if (prometheus !== 200) return new Response("down", { status: prometheus });
		const query = url.searchParams.get("query") ?? "";
		return Response.json(
			query.startsWith("cliproxy_quota_used_percent")
				? matrix("cliproxy_quota_used_percent", [
						[now - 3_600_000, 20],
						[now - 60_000, 32],
					])
				: matrix("cliproxy_quota_reset_timestamp_seconds", [
						[now - 3_600_000, (now + 86_400_000) / 1_000],
						[now - 60_000, (now + 86_400_000) / 1_000],
					]),
		);
	};
	return { fetch, calls };
}

function request(path: string, authorization: string | null = "Bearer management-key") {
	return new Request(`http://127.0.0.1${path}`, authorization === null ? {} : { headers: { authorization } });
}

describe("quota history handler", () => {
	test("serves pools keyed by the credential's auth index from Prometheus samples", async () => {
		const { fetch, calls } = stubFetch();
		const handle = createQuotaHistoryHandler({ prometheusUrl: PROMETHEUS, managementUrl: MANAGEMENT, fetch, now: () => now });
		const response = await handle(request(`/observability/quota/pools?provider=codex&from=${now - 7_200_000}&to=${now}`), "pools");

		expect(response.status).toBe(200);
		const body = (await response.json()) as { pools: { provider: string; burn: number; accounts: { account: string; name: string; remaining: number; window: string }[] }[] };
		expect(body.pools).toHaveLength(1);
		expect(body.pools[0]).toMatchObject({ provider: "codex", accountCount: 1, remaining: 68 });
		expect(body.pools[0]?.burn).toBeCloseTo(12 / (59 / 60), 6);
		expect(body.pools[0]?.accounts[0]).toMatchObject({ account: "7", name: "codex:7", window: "7d" });

		expect(calls[0]?.url.pathname).toBe("/v8/management/credentials");
		expect(calls[0]?.headers.get("authorization")).toBe("Bearer management-key");
		const queries = calls.slice(1).map((call) => call.url.searchParams.get("query"));
		expect(queries).toEqual([
			'cliproxy_quota_used_percent{provider="codex"}[7200s]',
			'cliproxy_quota_reset_timestamp_seconds{provider="codex"}[7200s]',
		]);
		expect(calls.slice(1).every((call) => !call.headers.has("authorization"))).toBe(true);
	});

	test("serves history with the backend's sample shape and default 24-hour range", async () => {
		const { fetch } = stubFetch();
		const handle = createQuotaHistoryHandler({ prometheusUrl: PROMETHEUS, managementUrl: MANAGEMENT, fetch, now: () => now });
		const body = (await (await handle(request("/observability/quota/history"), "history")).json()) as {
			from: number;
			to: number;
			history: Record<string, unknown[]>;
		};
		expect(body.from).toBe(now - 86_400_000);
		expect(body.to).toBe(now);
		expect(body.history["codex:7"]?.[1]).toEqual({
			provider: "codex",
			account: "7",
			at: now - 60_000,
			remaining: 68,
			resetAt: now + 86_400_000,
			window: "7d",
			periodHours: 168,
		});
	});

	test("passes management authentication failures through without querying Prometheus", async () => {
		const { fetch, calls } = stubFetch({ management: 401 });
		const handle = createQuotaHistoryHandler({ prometheusUrl: PROMETHEUS, managementUrl: MANAGEMENT, fetch, now: () => now });
		const response = await handle(request("/observability/quota/pools", null), "pools");
		expect(response.status).toBe(401);
		expect(calls).toHaveLength(1);
	});

	test("rejects invalid parameters before contacting anything", async () => {
		const { fetch, calls } = stubFetch();
		const handle = createQuotaHistoryHandler({ prometheusUrl: PROMETHEUS, managementUrl: MANAGEMENT, fetch, now: () => now });
		for (const path of [
			`/observability/quota/pools?to=${now + 1}`,
			`/observability/quota/pools?from=${now - 32 * 86_400_000}`,
			"/observability/quota/pools?rateHours=2",
			"/observability/quota/pools?usageMultiplier=3",
			'/observability/quota/pools?provider=codex"}',
		]) {
			expect((await handle(request(path), "pools")).status).toBe(400);
		}
		expect(calls).toHaveLength(0);
	});

	test("reports Prometheus failures as 502", async () => {
		const { fetch } = stubFetch({ prometheus: 503 });
		const handle = createQuotaHistoryHandler({ prometheusUrl: PROMETHEUS, managementUrl: MANAGEMENT, fetch, now: () => now });
		const response = await handle(request("/observability/quota/history"), "history");
		expect(response.status).toBe(502);
		expect(await response.json()).toEqual({ error: "quota history storage unavailable" });
	});
});
