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

	test("caches per provider until the TTL expires, and force bypasses it", async () => {
		await using dir = await credentialDirectory({ "a.json": codexAccounts["ready.json"] });
		let clock = 1_000;
		const { fetch, calls } = countingFetch(() => Response.json(codexPayload(5)));
		const reader = createQuotaReader({ credentialDirectory: dir.path, fetch, now: () => clock, cacheTtlMs: 15_000 });

		await reader.getQuota("codex");
		clock += 14_999;
		await reader.getQuota("codex");
		expect(calls).toHaveLength(1);

		await reader.getQuota("codex", { force: true });
		expect(calls).toHaveLength(2);

		clock += 15_000;
		await reader.getQuota("codex");
		expect(calls).toHaveLength(3);
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

	test("a caller abort cancels upstream requests", async () => {
		await using dir = await credentialDirectory({ "a.json": codexAccounts["ready.json"] });
		let upstreamSignal: AbortSignal | undefined;
		const fetch: Fetch = (_input, init) =>
			new Promise((_resolve, reject) => {
				upstreamSignal = init?.signal ?? undefined;
				init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
			});
		const controller = new AbortController();
		const pending = createQuotaReader({ credentialDirectory: dir.path, fetch }).getQuota("codex", {
			signal: controller.signal,
		});
		await Bun.sleep(10);
		controller.abort();
		await expect(pending).rejects.toThrow();
		expect(upstreamSignal?.aborted).toBe(true);
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
