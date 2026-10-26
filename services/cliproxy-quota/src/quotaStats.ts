export const HOUR_MS = 3_600_000;

export interface Sample {
	provider: string;
	account: string;
	at: number;
	remaining: number;
	resetAt: number | null;
	window: string;
	periodHours: number | null;
}

export interface Point {
	at: number;
	value: number;
}

export interface PoolAccount extends Sample {
	name: string;
	burn: number | null;
}

export interface Projection {
	available: boolean;
	points: Point[];
	refills: Point[];
	emptyAt: number | null;
	emptyAccounts: { name: string; at: number }[];
}

export interface Pool {
	provider: string;
	accountCount: number;
	capacity: number;
	remaining: number;
	burn: number | null;
	drain: number | null;
	points: Point[];
	accounts: PoolAccount[];
	projection: Projection;
}

const WINDOW_PERIOD_HOURS: Record<string, number> = { "7d": 168, "1d": 24, "24h": 24, "5h": 5, "1h": 1 };

export function windowPeriodHours(window: string): number | null {
	return WINDOW_PERIOD_HOURS[window] ?? null;
}

const SAME_RESET_TOLERANCE_MS = 60_000;

function sameReset(left: number | null, right: number | null): boolean {
	if (left === null || right === null) return left === right;
	return Math.abs(left - right) <= SAME_RESET_TOLERANCE_MS;
}

export function selectWindows(candidates: readonly Sample[]): Sample[] {
	const selected = new Map<string, Sample>();
	for (const sample of candidates) {
		const key = `${sample.provider}\u0000${sample.account}\u0000${sample.at}`;
		const current = selected.get(key);
		const period = sample.periodHours ?? 0;
		const currentPeriod = current?.periodHours ?? 0;
		if (
			current === undefined ||
			period > currentPeriod ||
			(period === currentPeriod &&
				sample.resetAt !== null &&
				(current.resetAt === null || sample.resetAt > current.resetAt))
		) {
			selected.set(key, sample);
		}
	}
	return [...selected.values()];
}

export function sampleName(sample: Pick<Sample, "provider" | "account">): string {
	return `${sample.provider}:${sample.account}`;
}

export function history(samples: readonly Sample[]): Record<string, Sample[]> {
	const grouped: Record<string, Sample[]> = {};
	for (const sample of samples) {
		const name = sampleName(sample);
		const series = grouped[name] ?? [];
		series.push(sample);
		grouped[name] = series;
	}
	for (const series of Object.values(grouped)) series.sort((left, right) => left.at - right.at);
	return grouped;
}

export function rate(samples: readonly Sample[], now: number, hours: number): number | null {
	const last = samples.at(-1);
	if (last === undefined) return null;
	const cutoff = now - hours * HOUR_MS;
	let elapsed = 0;
	let consumed = 0;
	for (let index = 1; index < samples.length; index++) {
		const before = samples[index - 1] as Sample;
		const after = samples[index] as Sample;
		if (
			before.at < cutoff ||
			after.at > now ||
			after.at <= before.at ||
			before.window !== after.window ||
			after.window !== last.window ||
			after.remaining > before.remaining ||
			(after.remaining !== before.remaining && !sameReset(before.resetAt, after.resetAt))
		) {
			continue;
		}
		if (before.resetAt !== null && before.resetAt > before.at && before.resetAt <= after.at) continue;
		elapsed += after.at - before.at;
		consumed += before.remaining - after.remaining;
	}
	return elapsed < 5 * 60_000 ? null : consumed / (elapsed / HOUR_MS);
}

export function pools(samples: readonly Sample[], now: number, hours: number, multiplier: number): Pool[] {
	const grouped = history(samples);
	const byProvider = new Map<string, PoolAccount[]>();
	for (const [name, series] of Object.entries(grouped)) {
		const last = series.at(-1) as Sample;
		const accounts = byProvider.get(last.provider) ?? [];
		accounts.push({ ...last, name, burn: rate(series, now, hours) });
		byProvider.set(last.provider, accounts);
	}

	return [...byProvider.entries()]
		.map(([provider, accounts]): Pool => {
			accounts.sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
			const known = accounts.every((account) => account.burn !== null);
			const burn = known ? accounts.reduce((total, account) => total + (account.burn ?? 0), 0) : null;
			return {
				provider,
				accountCount: accounts.length,
				capacity: accounts.length * 100,
				remaining: accounts.reduce((total, account) => total + account.remaining, 0),
				burn,
				drain: burn === null ? null : burn / accounts.length,
				points: recorded(accounts, grouped),
				accounts,
				projection: project(accounts, now, hours, multiplier),
			};
		})
		.sort((left, right) => (left.provider < right.provider ? -1 : left.provider > right.provider ? 1 : 0));
}

function recorded(accounts: readonly PoolAccount[], grouped: Record<string, Sample[]>): Point[] {
	let start = 0;
	let end = Number.MAX_SAFE_INTEGER;
	const events: Sample[] = [];
	for (const account of accounts) {
		const series = grouped[account.name] ?? [];
		start = Math.max(start, series[0]?.at ?? 0);
		end = Math.min(end, series.at(-1)?.at ?? 0);
		events.push(...series);
	}
	const points: Point[] = [];
	if (start > end) return points;
	events.sort((left, right) => left.at - right.at);
	const values = new Map<string, number>();
	let total = 0;
	for (let index = 0; index < events.length; ) {
		const at = (events[index] as Sample).at;
		if (at > end) break;
		while (index < events.length && (events[index] as Sample).at === at) {
			const sample = events[index] as Sample;
			const name = sampleName(sample);
			total += sample.remaining - (values.get(name) ?? 0);
			values.set(name, sample.remaining);
			index++;
		}
		if (at >= start) points.push({ at, value: total });
	}
	return points;
}

interface ProjectionEvent {
	at: number;
	refill: number;
	slope: number;
	empty: string | null;
}

const MAX_PROJECTION_EVENTS = 100_000;

function project(accounts: readonly PoolAccount[], now: number, hours: number, multiplier: number): Projection {
	const unavailable = (): Projection => ({
		available: false,
		points: [],
		refills: [],
		emptyAt: null,
		emptyAccounts: [],
	});
	const end = now + 14 * 24 * HOUR_MS;
	const events: ProjectionEvent[] = [];
	let total = 0;
	let slope = 0;

	for (const account of accounts) {
		if (
			account.burn === null ||
			account.resetAt === null ||
			account.resetAt <= now ||
			account.periodHours === null ||
			account.at < now - hours * HOUR_MS
		) {
			return unavailable();
		}
		const accountRate = (account.burn * multiplier) / HOUR_MS;
		total += account.remaining;
		if (account.remaining > 0) slope -= accountRate;

		let remaining = account.remaining;
		let cursor = now;
		let reset = account.resetAt;
		const period = Math.trunc(account.periodHours * HOUR_MS);
		let emptyRecorded = false;
		while (cursor < end) {
			const next = Math.min(reset, end);
			if (remaining === 0 || (accountRate > 0 && remaining / accountRate <= next - cursor)) {
				const event: ProjectionEvent = {
					at: remaining > 0 ? cursor + Math.ceil(remaining / accountRate) : cursor,
					refill: 0,
					slope: remaining > 0 ? accountRate : 0,
					empty: emptyRecorded ? null : account.name,
				};
				emptyRecorded = true;
				events.push(event);
				remaining = 0;
			} else {
				remaining -= accountRate * (next - cursor);
			}
			if (next === reset) {
				events.push({ at: reset, refill: 100 - remaining, slope: remaining === 0 ? -accountRate : 0, empty: null });
				remaining = 100;
				reset += period;
			}
			cursor = next;
			if (events.length > MAX_PROJECTION_EVENTS) return unavailable();
		}
	}

	const projection: Projection = { ...unavailable(), available: true, points: [{ at: now, value: total }] };
	if (total === 0) projection.emptyAt = now;
	events.sort((left, right) => left.at - right.at);
	let cursor = now;
	for (let index = 0; index < events.length; ) {
		const at = (events[index] as ProjectionEvent).at;
		total = Math.max(0, total + slope * (at - cursor));
		projection.points.push({ at, value: total });
		if (total < 1e-7 && projection.emptyAt === null) projection.emptyAt = at;
		let refill = 0;
		while (index < events.length && (events[index] as ProjectionEvent).at === at) {
			const event = events[index] as ProjectionEvent;
			refill += event.refill;
			slope += event.slope;
			if (event.empty !== null) projection.emptyAccounts.push({ name: event.empty, at });
			index++;
		}
		if (refill > 0) {
			total += refill;
			projection.refills.push({ at, value: refill });
			projection.points.push({ at, value: total });
		}
		cursor = at;
	}
	if (cursor < end) projection.points.push({ at: end, value: Math.max(0, total + slope * (end - cursor)) });
	return projection;
}
