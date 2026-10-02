/** Provider quota formats, parsed into one window/readiness shape. */

export type QuotaProvider = "claude" | "codex" | "xai";

export const QUOTA_PROVIDERS: readonly QuotaProvider[] = ["claude", "codex", "xai"];

export interface QuotaWindow {
	label: string;
	usedPercent: number;
	resetAtMs: number | null;
}

/** Response body of `/quota/v1/{provider}`. Field names are a public contract. */
export interface ProviderQuota {
	provider: QuotaProvider;
	windows: QuotaWindow[];
	readyAccounts: number;
	totalAccounts: number;
	fetchedAtMs: number;
}

export interface AccountQuota {
	windows: QuotaWindow[];
	ready: boolean;
}

export function isQuotaProvider(value: unknown): value is QuotaProvider {
	return value === "claude" || value === "codex" || value === "xai";
}

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function finiteNumber(value: unknown): number | null {
	if (typeof value === "number" && Number.isFinite(value)) return value;
	if (typeof value !== "string" || value.trim().length === 0) return null;
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : null;
}

function percentage(value: unknown): number | null {
	const parsed = finiteNumber(value);
	return parsed === null ? null : Math.max(0, Math.min(100, parsed));
}

function timestampMs(value: unknown): number | null {
	if (typeof value === "string" && value.trim().length > 0) {
		const parsed = Date.parse(value);
		if (Number.isFinite(parsed)) return parsed;
	}
	const parsed = finiteNumber(value);
	if (parsed === null || parsed <= 0) return null;
	return parsed < 100_000_000_000 ? parsed * 1_000 : parsed;
}

function durationLabel(seconds: number | null): string {
	if (seconds === null || seconds <= 0) return "usage";
	if (seconds === 18_000) return "5h";
	if (seconds === 604_800) return "7d";
	if (seconds >= 2_419_200 && seconds <= 2_678_400) return "30d";
	if (seconds % 86_400 === 0) return `${seconds / 86_400}d`;
	if (seconds % 3_600 === 0) return `${seconds / 3_600}h`;
	return "usage";
}

function parseCodexWindow(value: unknown): QuotaWindow | null {
	if (!isRecord(value)) return null;
	const usedPercent = percentage(value.used_percent ?? value.usedPercent);
	if (usedPercent === null) return null;
	return {
		label: durationLabel(finiteNumber(value.limit_window_seconds ?? value.limitWindowSeconds)),
		usedPercent,
		resetAtMs: timestampMs(value.reset_at ?? value.resetAt),
	};
}

export function parseCodexQuota(payload: unknown): AccountQuota {
	if (!isRecord(payload)) {
		throw new Error("Codex quota response must be an object");
	}
	const rateLimit = payload.rate_limit ?? payload.rateLimit;
	if (!isRecord(rateLimit)) {
		throw new Error("Codex quota response must contain rate_limit");
	}

	const windows = [
		parseCodexWindow(rateLimit.primary_window ?? rateLimit.primaryWindow),
		parseCodexWindow(rateLimit.secondary_window ?? rateLimit.secondaryWindow),
	].filter((window): window is QuotaWindow => window !== null);
	if (windows.length === 0) {
		throw new Error("Codex quota response contains no usage windows");
	}

	const order = new Map([
		["5h", 0],
		["7d", 1],
		["30d", 2],
	]);
	const allowed = rateLimit.allowed;
	const limitReached = rateLimit.limit_reached ?? rateLimit.limitReached;
	return {
		windows: windows.sort(
			(left, right) => (order.get(left.label) ?? 3) - (order.get(right.label) ?? 3),
		),
		ready:
			allowed !== false &&
			limitReached !== true &&
			windows.every((window) => window.usedPercent < 100),
	};
}

function parseClaudeWindow(value: unknown, label: string): QuotaWindow | null {
	if (!isRecord(value)) return null;
	const usedPercent = percentage(value.utilization);
	if (usedPercent === null) return null;
	return {
		label,
		usedPercent,
		resetAtMs: timestampMs(value.resets_at ?? value.resetsAt),
	};
}

export function parseClaudeQuota(payload: unknown): AccountQuota {
	if (!isRecord(payload)) {
		throw new Error("Claude quota response must be an object");
	}
	const windows = [
		parseClaudeWindow(payload.five_hour ?? payload.fiveHour, "5h"),
		parseClaudeWindow(payload.seven_day ?? payload.sevenDay, "7d"),
	].filter((window): window is QuotaWindow => window !== null);
	if (windows.length === 0) {
		throw new Error("Claude quota response contains no usage windows");
	}
	return {
		windows,
		ready: windows.every((window) => window.usedPercent < 100),
	};
}

export function parseXAIQuota(payload: unknown): AccountQuota {
	if (!isRecord(payload)) {
		throw new Error("xAI quota response must be an object");
	}
	const config = isRecord(payload.config) ? payload.config : payload;
	const usedPercent = percentage(config.creditUsagePercent ?? config.credit_usage_percent);
	if (usedPercent === null) {
		throw new Error("xAI quota response must contain creditUsagePercent");
	}

	const period = isRecord(config.currentPeriod)
		? config.currentPeriod
		: isRecord(config.current_period)
			? config.current_period
			: null;
	const startMs = timestampMs(period?.start);
	const endMs = timestampMs(period?.end);
	const durationSeconds =
		startMs !== null && endMs !== null && endMs > startMs
			? Math.round((endMs - startMs) / 1_000)
			: null;
	const periodType = typeof period?.type === "string" ? period.type.toLowerCase() : "";
	const label = periodType.includes("weekly") ? "7d" : durationLabel(durationSeconds);

	return {
		windows: [{ label, usedPercent, resetAtMs: endMs }],
		ready: usedPercent < 100,
	};
}

function isQuotaWindow(value: unknown): value is QuotaWindow {
	return (
		isRecord(value) &&
		typeof value.label === "string" &&
		typeof value.usedPercent === "number" &&
		value.usedPercent >= 0 &&
		value.usedPercent <= 100 &&
		(value.resetAtMs === null ||
			(typeof value.resetAtMs === "number" && Number.isFinite(value.resetAtMs)))
	);
}

/** Validate a `/quota/v1/{provider}` body received over HTTP. */
export function parseProviderQuota(value: unknown, provider: QuotaProvider): ProviderQuota {
	if (!isRecord(value)) {
		throw new Error(`${provider} quota response is invalid`);
	}
	const { windows, readyAccounts, totalAccounts, fetchedAtMs } = value;
	if (
		value.provider !== provider ||
		!Array.isArray(windows) ||
		windows.length === 0 ||
		!windows.every(isQuotaWindow) ||
		typeof readyAccounts !== "number" ||
		!Number.isInteger(readyAccounts) ||
		readyAccounts < 0 ||
		typeof totalAccounts !== "number" ||
		!Number.isInteger(totalAccounts) ||
		totalAccounts < readyAccounts ||
		typeof fetchedAtMs !== "number" ||
		!Number.isFinite(fetchedAtMs)
	) {
		throw new Error(`${provider} quota response is invalid`);
	}
	return { provider, windows, readyAccounts, totalAccounts, fetchedAtMs };
}
