import { describe, expect, test } from "bun:test";
import {
	CLAUDE_USAGE_URL,
	CODEX_USAGE_URL,
	XAI_USAGE_URL,
	createQuotaReader,
	type Fetch,
} from "../src/credentials.ts";
import { codexPayload, credentialDirectory } from "./fixtures.ts";

const codexAccounts = {
	"ready.json": { type: "codex", access_token: "ready-token", account_id: "ready-account" },
	"exhausted.json": { type: "codex", access_token: "exhausted-token", account_id: "exhausted-account" },
};

function countingFetch(respond: (url: string, headers: Headers) => Response | Promise<Response>) {
	const calls: { url: string; headers: Headers }[] = [];
	const fetch: Fetch = async (input, init) => {
		const call = { url: String(input), headers: new Headers(init?.headers) };
		calls.push(call);
		return respond(call.url, call.headers);
	};
	return { fetch, calls };
}

describe("credential reader", () => {
	test("reports the least-used ready account across the pool", async () => {
		await using dir = await credentialDirectory(codexAccounts);
		const { fetch, calls } = countingFetch((_url, headers) =>
			Response.json(
				headers.get("chatgpt-account-id") === "ready-account" ? codexPayload(5) : codexPayload(100, { allowed: false }),
			),
		);
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => 123_456 });

		expect(await reader.getQuota("codex")).toEqual({
			provider: "codex",
			windows: [{ label: "7d", usedPercent: 5, resetAtMs: 1_788_454_137_000 }],
			readyAccounts: 1,
			totalAccounts: 2,
			fetchedAtMs: 123_456,
		});
		expect(calls).toHaveLength(2);
		expect(calls.every((call) => call.url === CODEX_USAGE_URL)).toBe(true);
	});

	test("falls back to the least-used account when none is ready", async () => {
		await using dir = await credentialDirectory(codexAccounts);
		const { fetch } = countingFetch((_url, headers) =>
			Response.json(
				headers.get("chatgpt-account-id") === "ready-account"
					? codexPayload(100)
					: codexPayload(80, { limitReached: true }),
			),
		);
		const quota = await createQuotaReader({ credentialDirectory: dir.path, fetch }).getQuota("codex");
		expect(quota?.readyAccounts).toBe(0);
		expect(quota?.windows[0]?.usedPercent).toBe(80);
	});

	test("caches each account until the TTL expires", async () => {
		await using dir = await credentialDirectory({ "a.json": codexAccounts["ready.json"] });
		let clock = 1_000;
		const { fetch, calls } = countingFetch(() => Response.json(codexPayload(5)));
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => clock, cacheTtlMs: 60_000 });

		expect((await reader.getQuota("codex"))?.fetchedAtMs).toBe(1_000);
		clock += 59_999;
		expect((await reader.getQuota("codex"))?.fetchedAtMs).toBe(1_000);
		expect(calls).toHaveLength(1);

		clock += 1;
		expect((await reader.getQuota("codex"))?.fetchedAtMs).toBe(61_000);
		expect(calls).toHaveLength(2);
	});

	test("metrics and provider requests share one per-account cache", async () => {
		await using dir = await credentialDirectory({ "claude-a.json": { type: "claude", access_token: "a" } });
		const { fetch, calls } = countingFetch(() => Response.json({ five_hour: { utilization: 10 } }));
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => 0 });

		await reader.getAccountQuotas();
		await reader.getQuota("claude");
		await reader.getAccountQuotas();
		expect(calls).toHaveLength(1);
	});

	test("concurrent callers share one upstream request", async () => {
		await using dir = await credentialDirectory({ "claude-a.json": { type: "claude", access_token: "a" } });
		const { fetch, calls } = countingFetch(async () => {
			await Bun.sleep(10);
			return Response.json({ five_hour: { utilization: 10 } });
		});
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch });

		const results = await Promise.all([
			reader.getQuota("claude"),
			reader.getQuota("claude"),
			reader.getAccountQuotas(),
		]);
		expect(calls).toHaveLength(1);
		expect(results[0]).toEqual(results[1]);
	});

	test("a rate-limited account waits for Retry-After and serves its last answer meanwhile", async () => {
		await using dir = await credentialDirectory({ "claude-a.json": { type: "claude", access_token: "a" } });
		let clock = 0;
		let limited = false;
		const logged: string[] = [];
		const { fetch, calls } = countingFetch(() =>
			limited
				? new Response(null, { status: 429, headers: { "retry-after": "600" } })
				: Response.json({ five_hour: { utilization: 10 } }),
		);
		const reader = createQuotaReader({
			credentialDirectory: dir.path,
			fetch,
			now: () => clock,
			cacheTtlMs: 60_000,
			log: (message) => logged.push(message),
		});

		await reader.getQuota("claude");
		limited = true;
		clock = 60_000;
		expect((await reader.getQuota("claude"))?.fetchedAtMs).toBe(0);
		expect(logged).toEqual(["cliproxy-quota: claude account claude-a: HTTP 429; next request in 600s"]);
		expect((await reader.getAccountQuotas())[0]).toMatchObject({ lastFetchFailed: true, quota: { fetchedAtMs: 0 } });

		clock = 659_999;
		await reader.getQuota("claude");
		expect(calls).toHaveLength(2);

		limited = false;
		clock = 660_000;
		expect((await reader.getQuota("claude"))?.fetchedAtMs).toBe(660_000);
		expect(calls).toHaveLength(3);
		expect((await reader.getAccountQuotas())[0]?.lastFetchFailed).toBe(false);
	});

	test("backs off exponentially without Retry-After and honours an HTTP-date Retry-After", async () => {
		await using dir = await credentialDirectory({ "codex.json": codexAccounts["ready.json"] });
		let clock = 0;
		let respond = () => new Response(null, { status: 503 });
		const logged: string[] = [];
		const { fetch } = countingFetch(() => respond());
		const reader = createQuotaReader({
			credentialDirectory: dir.path,
			fetch,
			now: () => clock,
			log: (message) => logged.push(message),
		});

		for (const at of [0, 30_000, 90_000]) {
			clock = at;
			await expect(reader.getQuota("codex")).rejects.toThrow(/failed for every account/);
		}
		respond = () =>
			new Response(null, { status: 429, headers: { "retry-after": new Date(210_000 + 900_000).toUTCString() } });
		clock = 210_000;
		await expect(reader.getQuota("codex")).rejects.toThrow();

		expect(logged.map((message) => message.replace(/^.*; /, ""))).toEqual([
			"next request in 30s",
			"next request in 60s",
			"next request in 120s",
			"next request in 900s",
		]);
	});

	test("an account with no answer during its backoff fails without contacting the provider", async () => {
		await using dir = await credentialDirectory({ "codex.json": codexAccounts["ready.json"] });
		let clock = 0;
		const { fetch, calls } = countingFetch(() => new Response(null, { status: 429 }));
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => clock, log: () => {} });

		await expect(reader.getQuota("codex")).rejects.toThrow(/failed for every account/);
		clock = 29_999;
		await expect(reader.getQuota("codex")).rejects.toThrow(/failed for every account/);
		expect(calls).toHaveLength(1);
	});

	test("stops serving an answer once it is older than the stale limit", async () => {
		await using dir = await credentialDirectory({ "codex.json": codexAccounts["ready.json"] });
		let clock = 0;
		let fail = false;
		const { fetch } = countingFetch(() =>
			fail ? new Response(null, { status: 429, headers: { "retry-after": "3600" } }) : Response.json(codexPayload(5)),
		);
		const reader = createQuotaReader({
			credentialDirectory: dir.path,
			fetch,
			now: () => clock,
			maxStaleMs: 1_800_000,
			log: () => {},
		});

		await reader.getQuota("codex");
		fail = true;
		clock = 1_799_999;
		expect(await reader.getQuota("codex")).not.toBeNull();
		clock = 1_800_000;
		await expect(reader.getQuota("codex")).rejects.toThrow(/failed for every account/);
	});

	test("a refreshed access token is tried without waiting out the old token's backoff", async () => {
		await using dir = await credentialDirectory({ "codex.json": { type: "codex", access_token: "expired" } });
		const { fetch, calls } = countingFetch((_url, headers) =>
			headers.get("authorization") === "Bearer expired"
				? new Response(null, { status: 401 })
				: Response.json(codexPayload(5)),
		);
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => 0, log: () => {} });

		await expect(reader.getQuota("codex")).rejects.toThrow();
		await Bun.write(`${dir.path}/codex.json`, JSON.stringify({ type: "codex", access_token: "fresh" }));
		expect(await reader.getQuota("codex")).not.toBeNull();
		expect(calls.map((call) => call.headers.get("authorization"))).toEqual(["Bearer expired", "Bearer fresh"]);
	});

	test("returns null without enabled credentials and skips unusable files", async () => {
		await using dir = await credentialDirectory({
			"disabled.json": { type: "codex", access_token: "t", disabled: true },
			"no-token.json": { type: "codex", access_token: "" },
			"other.json": { type: "kimi", access_token: "t" },
			"broken.json": "{not json",
			"notes.txt": "ignored",
		});
		const { fetch, calls } = countingFetch(() => Response.json(codexPayload(5)));
		expect(await createQuotaReader({ credentialDirectory: dir.path, fetch }).getQuota("codex")).toBeNull();
		expect(calls).toHaveLength(0);
	});

	test("a missing credential directory means no credentials", async () => {
		const { fetch } = countingFetch(() => Response.json(codexPayload(5)));
		const reader = createQuotaReader({ credentialDirectory: "/nonexistent/cliproxy-quota-test", fetch });
		expect(await reader.getQuota("claude")).toBeNull();
	});

	test("partial upstream failures still report the accounts that answered", async () => {
		await using dir = await credentialDirectory(codexAccounts);
		const { fetch } = countingFetch((_url, headers) =>
			headers.get("chatgpt-account-id") === "ready-account"
				? Response.json(codexPayload(30))
				: new Response("nope", { status: 401 }),
		);
		const quota = await createQuotaReader({ credentialDirectory: dir.path, fetch }).getQuota("codex");
		expect(quota).toMatchObject({ readyAccounts: 1, totalAccounts: 2 });
	});

	test("throws when every account fails, including malformed bodies", async () => {
		await using dir = await credentialDirectory(codexAccounts);
		for (const respond of [
			() => new Response(null, { status: 500 }),
			() => Response.json({ unexpected: true }),
			() => new Response("not json"),
		]) {
			const { fetch } = countingFetch(respond);
			await expect(createQuotaReader({ credentialDirectory: dir.path, fetch }).getQuota("codex")).rejects.toThrow(
				/failed for every account/,
			);
		}
	});

	test("times out a hung upstream", async () => {
		await using dir = await credentialDirectory({ "a.json": codexAccounts["ready.json"] });
		const fetch: Fetch = (_input, init) =>
			new Promise((_resolve, reject) => {
				init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
			});
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, requestTimeoutMs: 50 });
		const started = performance.now();
		await expect(reader.getQuota("codex")).rejects.toThrow(/failed for every account/);
		expect(performance.now() - started).toBeLessThan(2_000);
	});

	test("a caller abort stops waiting but leaves the shared request to finish", async () => {
		await using dir = await credentialDirectory({ "a.json": codexAccounts["ready.json"] });
		let upstreamSignal: AbortSignal | undefined;
		let answer: ((response: Response) => void) | undefined;
		const fetch: Fetch = (_input, init) => {
			upstreamSignal = init?.signal ?? undefined;
			return new Promise((resolve) => {
				answer = resolve;
			});
		};
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch });
		const controller = new AbortController();
		const pending = reader.getQuota("codex", { signal: controller.signal });
		await Bun.sleep(10);
		controller.abort();
		await expect(pending).rejects.toThrow();
		expect(upstreamSignal?.aborted).toBe(false);

		answer?.(Response.json(codexPayload(5)));
		expect((await reader.getQuota("codex"))?.windows[0]?.usedPercent).toBe(5);
	});

	test("reports every account across providers, marking failures without throwing", async () => {
		await using dir = await credentialDirectory({
			...codexAccounts,
			"claude-main.json": { type: "claude", access_token: "claude-token" },
			"disabled.json": { type: "xai", access_token: "t", disabled: true },
		});
		const { fetch } = countingFetch((url, headers) => {
			if (url === CLAUDE_USAGE_URL) return Response.json({ seven_day: { utilization: 12 } });
			return headers.get("chatgpt-account-id") === "ready-account"
				? Response.json(codexPayload(5))
				: new Response(null, { status: 401 });
		});
		const results = await createQuotaReader({
			credentialDirectory: dir.path,
			fetch,
			now: () => 0,
			log: () => {},
		}).getAccountQuotas();

		expect(results.sort((left, right) => left.account.localeCompare(right.account))).toEqual([
			{
				provider: "claude",
				account: "claude-main",
				lastFetchFailed: false,
				quota: { fetchedAtMs: 0, ready: true, windows: [{ label: "7d", usedPercent: 12, resetAtMs: null }] },
			},
			{ provider: "codex", account: "exhausted", lastFetchFailed: true, quota: null },
			{
				provider: "codex",
				account: "ready",
				lastFetchFailed: false,
				quota: {
					fetchedAtMs: 0,
					ready: true,
					windows: [{ label: "7d", usedPercent: 5, resetAtMs: 1_788_454_137_000 }],
				},
			},
		]);
	});

	test("sends each provider's endpoint and token, and never other providers' tokens", async () => {
		await using dir = await credentialDirectory({
			"claude.json": { type: "claude", access_token: "claude-token" },
			"xai.json": { type: "xai", access_token: "xai-token" },
			"codex.json": { type: "codex", access_token: "codex-token" },
		});
		const { fetch, calls } = countingFetch((url) =>
			url === CLAUDE_USAGE_URL
				? Response.json({ five_hour: { utilization: 1 } })
				: Response.json({ creditUsagePercent: 2 }),
		);
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch });
		await reader.getQuota("claude");
		await reader.getQuota("xai");

		expect(calls.map((call) => [call.url, call.headers.get("authorization")])).toEqual([
			[CLAUDE_USAGE_URL, "Bearer claude-token"],
			[XAI_USAGE_URL, "Bearer xai-token"],
		]);
		expect(calls[0]?.headers.get("anthropic-beta")).toBe("oauth-2025-04-20");
		expect(calls[1]?.headers.get("x-xai-token-auth")).toBe("xai-grok-cli");
	});
});
