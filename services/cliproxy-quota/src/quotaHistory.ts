import type { Fetch } from "./credentials.ts";
import { history, pools, selectWindows, windowPeriodHours, type Sample } from "./quotaStats.ts";

export const QUOTA_HISTORY_PATH = /^\/observability\/quota\/(history|pools)$/;

const DAY_MS = 86_400_000;
const PROVIDER = /^[a-z0-9_-]+$/;

export interface QuotaHistoryOptions {
	prometheusUrl: string;
	managementUrl: string;
	fetch?: Fetch;
	now?: () => number;
}

interface PrometheusSeries {
	metric: Record<string, string>;
	values: [number, string][];
}

class HttpError extends Error {
	constructor(
		readonly status: number,
		message: string,
	) {
		super(message);
	}
}

function json(status: number, body: unknown): Response {
	return Response.json(body, { status, headers: { "cache-control": "no-store" } });
}

function parseQuery(url: URL, now: number) {
	const integer = (key: string, fallback: number) => {
		const value = url.searchParams.get(key);
		return value === null ? fallback : /^-?\d+$/.test(value) ? Number(value) : Number.NaN;
	};
	const to = integer("to", now);
	if (!(to > 0 && to <= now)) {
		throw new HttpError(400, "to must be a positive epoch millisecond timestamp not in the future");
	}
	const from = integer("from", to - DAY_MS);
	if (!(from > 0 && from < to && to - from <= 31 * DAY_MS)) {
		throw new HttpError(400, "from/to must delimit a positive interval of at most 31 days");
	}
	const rateHours = Number(url.searchParams.get("rateHours") ?? "3");
	if (rateHours !== 1 && rateHours !== 3 && rateHours !== 6) {
		throw new HttpError(400, "rateHours must be 1, 3 or 6");
	}
	const usageMultiplier = Number(url.searchParams.get("usageMultiplier") ?? "1");
	if (![0, 0.5, 1, 2].includes(usageMultiplier)) {
		throw new HttpError(400, "usageMultiplier must be 0, 0.5, 1 or 2");
	}
	const provider = url.searchParams.get("provider") || null;
	if (provider !== null && !PROVIDER.test(provider)) {
		throw new HttpError(400, "provider is invalid");
	}
	return { from, to, rateHours, usageMultiplier, provider, account: url.searchParams.get("account") || null };
}

async function authIndexByFile(
	fetchImplementation: Fetch,
	managementUrl: string,
	request: Request,
): Promise<Map<string, string>> {
	const headers: Record<string, string> = {};
	const authorization = request.headers.get("authorization");
	if (authorization !== null) headers.authorization = authorization;
	const response = await fetchImplementation(`${managementUrl}/v8/management/credentials`, {
		headers,
		signal: request.signal,
	});
	if (response.status === 401 || response.status === 403 || response.status === 404) {
		throw new HttpError(response.status, "management authentication failed");
	}
	if (!response.ok) throw new HttpError(502, "credential inventory unavailable");
	const body: unknown = await response.json();
	const files = typeof body === "object" && body !== null && "files" in body ? body.files : null;
	const accounts = new Map<string, string>();
	if (!Array.isArray(files)) return accounts;
	for (const file of files) {
		if (typeof file !== "object" || file === null) continue;
		const { name, auth_index: authIndex } = file as Record<string, unknown>;
		if (typeof name === "string" && (typeof authIndex === "string" || typeof authIndex === "number")) {
			accounts.set(name.replace(/\.json$/, ""), String(authIndex));
		}
	}
	return accounts;
}

async function querySeries(
	fetchImplementation: Fetch,
	prometheusUrl: string,
	metric: string,
	query: ReturnType<typeof parseQuery>,
	signal: AbortSignal,
): Promise<PrometheusSeries[]> {
	const selector = query.provider === null ? "" : `{provider="${query.provider}"}`;
	const seconds = Math.ceil((query.to - query.from) / 1_000);
	const url = new URL(`${prometheusUrl}/api/v1/query`);
	url.searchParams.set("query", `${metric}${selector}[${seconds}s]`);
	url.searchParams.set("time", String(query.to / 1_000));
	const response = await fetchImplementation(url.href, { headers: {}, signal });
	if (!response.ok) throw new HttpError(502, "quota history storage unavailable");
	const body = (await response.json()) as { status?: string; data?: { result?: PrometheusSeries[] } };
	if (body.status !== "success" || !Array.isArray(body.data?.result)) {
		throw new HttpError(502, "quota history storage unavailable");
	}
	return body.data.result;
}

function seriesKey(metric: Record<string, string>, at: number): string {
	return [metric.provider, metric.account, metric.window, at].join("\u0000");
}

export function samplesFromSeries(
	used: readonly PrometheusSeries[],
	resets: readonly PrometheusSeries[],
	authIndexes: ReadonlyMap<string, string>,
): Sample[] {
	const resetAt = new Map<string, number>();
	for (const series of resets) {
		for (const [seconds, value] of series.values) {
			resetAt.set(seriesKey(series.metric, Math.round(seconds * 1_000)), Number(value) * 1_000);
		}
	}
	const candidates: Sample[] = [];
	for (const { metric, values } of used) {
		const { provider, account, window } = metric;
		if (provider === undefined || account === undefined || window === undefined) continue;
		for (const [seconds, value] of values) {
			const at = Math.round(seconds * 1_000);
			const usedPercent = Number(value);
			if (!Number.isFinite(usedPercent)) continue;
			candidates.push({
				provider,
				account: authIndexes.get(account) ?? account,
				at,
				remaining: Math.max(0, Math.min(100, 100 - usedPercent)),
				resetAt: resetAt.get(seriesKey(metric, at)) ?? null,
				window,
				periodHours: windowPeriodHours(window),
			});
		}
	}
	return selectWindows(candidates);
}

export function createQuotaHistoryHandler(
	options: QuotaHistoryOptions,
): (request: Request, kind: "history" | "pools") => Promise<Response> {
	const fetchImplementation = options.fetch ?? globalThis.fetch;
	const now = options.now ?? Date.now;
	return async (request, kind) => {
		try {
			const query = parseQuery(new URL(request.url), now());
			const authIndexes = await authIndexByFile(fetchImplementation, options.managementUrl, request);
			const [used, resets] = await Promise.all(
				["cliproxy_quota_used_percent", "cliproxy_quota_reset_timestamp_seconds"].map((metric) =>
					querySeries(fetchImplementation, options.prometheusUrl, metric, query, request.signal),
				),
			);
			const samples = samplesFromSeries(used ?? [], resets ?? [], authIndexes).filter(
				(sample) =>
					sample.at >= query.from &&
					sample.at <= query.to &&
					(query.account === null || sample.account === query.account),
			);
			const { from, to, rateHours, usageMultiplier } = query;
			return kind === "pools"
				? json(200, { from, to, rateHours, usageMultiplier, pools: pools(samples, to, rateHours, usageMultiplier) })
				: json(200, { from, to, history: history(samples) });
		} catch (error) {
			if (error instanceof HttpError) return json(error.status, { error: error.message });
			return json(502, { error: "quota history storage unavailable" });
		}
	};
}
