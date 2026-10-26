import { describe, expect, test } from "bun:test";
import { HOUR_MS, history, pools, rate, selectWindows, type Sample } from "../src/quotaStats.ts";

const now = 1_800_000_000_000;

function sample(overrides: Partial<Sample>): Sample {
	return {
		provider: "codex",
		account: "1",
		at: now,
		remaining: 50,
		resetAt: now + 24 * HOUR_MS,
		window: "7d",
		periodHours: 168,
		...overrides,
	};
}

describe("quota statistics model", () => {
	test("two observations 80% to 68% over an hour give a burn of 12 and an available forecast", () => {
		const [pool] = pools(
			[sample({ at: now - HOUR_MS, remaining: 80 }), sample({ at: now, remaining: 68 })],
			now,
			3,
			1,
		);
		expect(pool).toMatchObject({ provider: "codex", accountCount: 1, capacity: 100, remaining: 68, burn: 12, drain: 12 });
		expect(pool?.points).toEqual([
			{ at: now - HOUR_MS, value: 80 },
			{ at: now, value: 68 },
		]);
		expect(pool?.projection.available).toBe(true);
		expect(pool?.projection.emptyAt).toBe(now + Math.ceil(68 / (12 / HOUR_MS)));
		expect(pool?.projection.emptyAccounts).toEqual([{ name: "codex:1", at: now + Math.ceil(68 / (12 / HOUR_MS)) }]);
		expect(pool?.projection.refills[0]).toEqual({ at: now + 24 * HOUR_MS, value: 100 });
	});

	test("rates skip refills, reset crossings and window changes, and need five minutes of evidence", () => {
		expect(rate([sample({ at: now - 4 * 60_000, remaining: 60 }), sample({ remaining: 50 })], now, 3)).toBeNull();
		expect(rate([sample({ at: now - HOUR_MS, remaining: 10 }), sample({ remaining: 90 })], now, 3)).toBeNull();
		expect(
			rate([sample({ at: now - HOUR_MS, remaining: 60, window: "5h" }), sample({ remaining: 50 })], now, 3),
		).toBeNull();
		expect(
			rate([sample({ at: now - HOUR_MS, remaining: 60, resetAt: now - 1 }), sample({ remaining: 50, resetAt: now - 1 })], now, 3),
		).toBeNull();
	});

	test("reset timestamps a few seconds apart still count as the same window", () => {
		expect(
			rate(
				[sample({ at: now - HOUR_MS, remaining: 60, resetAt: now + HOUR_MS }), sample({ remaining: 50, resetAt: now + HOUR_MS + 2_000 })],
				now,
				3,
			),
		).toBe(10);
	});

	test("an unused window whose reset moves with the clock has a zero rate", () => {
		expect(
			rate(
				[
					sample({ at: now - HOUR_MS, remaining: 100, resetAt: now - HOUR_MS + 168 * HOUR_MS }),
					sample({ remaining: 100, resetAt: now + 168 * HOUR_MS }),
				],
				now,
				3,
			),
		).toBe(0);
	});

	test("an account without a known rate makes the provider rate and projection unavailable", () => {
		const [pool] = pools([sample({ account: "1" }), sample({ account: "2", at: now - HOUR_MS }), sample({ account: "2" })], now, 3, 1);
		expect(pool?.burn).toBeNull();
		expect(pool?.drain).toBeNull();
		expect(pool?.projection).toEqual({ available: false, points: [], refills: [], emptyAt: null, emptyAccounts: [] });
	});

	test("selects the longest recognized window per account and time", () => {
		const selected = selectWindows([
			sample({ window: "5h", periodHours: 5, remaining: 10 }),
			sample({ window: "7d", periodHours: 168, remaining: 70 }),
			sample({ window: "usage", periodHours: null, remaining: 99 }),
		]);
		expect(selected.map((entry) => entry.window)).toEqual(["7d"]);
	});

	test("history is keyed by provider and account, in time order", () => {
		expect(history([sample({ at: now }), sample({ at: now - 1 })])["codex:1"]?.map((entry) => entry.at)).toEqual([
			now - 1,
			now,
		]);
	});
});
