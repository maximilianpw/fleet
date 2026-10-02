/** `cliproxyapi-util quota` report. The JSON shape (`version: 1`) is stable. */
import type { QuotaClient } from "./client.ts";
import type { ProviderQuota, QuotaProvider } from "./providers.ts";

export type RouteFamily = "codex" | "claude" | "grok";
export type Availability = "available" | "unavailable" | "unknown";

export interface QuotaReportEntry {
	family: RouteFamily;
	status: Availability;
	usedPercent: number | null;
	readyAccounts: number;
	totalAccounts: number;
}

export interface QuotaReport {
	version: 1;
	families: QuotaReportEntry[];
}

const ROUTE_FAMILIES: readonly { family: RouteFamily; provider: QuotaProvider }[] = [
	{ family: "codex", provider: "codex" },
	{ family: "claude", provider: "claude" },
	{ family: "grok", provider: "xai" },
];

function entry(family: RouteFamily, quota: ProviderQuota | null): QuotaReportEntry {
	if (quota === null) {
		return { family, status: "unavailable", usedPercent: null, readyAccounts: 0, totalAccounts: 0 };
	}
	return {
		family,
		status: quota.readyAccounts > 0 ? "available" : "unavailable",
		usedPercent: Math.max(...quota.windows.map((window) => window.usedPercent)),
		readyAccounts: quota.readyAccounts,
		totalAccounts: quota.totalAccounts,
	};
}

export async function getQuotaReport(client: QuotaClient): Promise<QuotaReport> {
	const families = await Promise.all(
		ROUTE_FAMILIES.map(async ({ family, provider }): Promise<QuotaReportEntry> => {
			try {
				return entry(family, await client.getQuota(provider));
			} catch {
				return { family, status: "unknown", usedPercent: null, readyAccounts: 0, totalAccounts: 0 };
			}
		}),
	);
	return { version: 1, families };
}

function percentageText(value: number | null): string {
	if (value === null) return "-";
	return `${Number.isInteger(value) ? value : value.toFixed(1)}%`;
}

function accountsText(entry: QuotaReportEntry): string {
	return entry.totalAccounts === 0 ? "-" : `${entry.readyAccounts}/${entry.totalAccounts}`;
}

export function formatQuotaReport(report: QuotaReport): string {
	const rows = [
		["family", "status", "usage", "accounts"],
		...report.families.map((entry) => [
			entry.family,
			entry.status,
			percentageText(entry.usedPercent),
			accountsText(entry),
		]),
	];
	const widths = rows[0]?.map((_, column) => Math.max(...rows.map((row) => row[column]?.length ?? 0))) ?? [];
	return rows
		.map((row) => row.map((cell, column) => cell.padEnd(widths[column] ?? 0)).join("  ").trimEnd())
		.join("\n");
}
