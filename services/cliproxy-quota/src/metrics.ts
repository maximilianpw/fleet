import type { AccountQuotaResult } from "./credentials.ts";

export const METRICS_CONTENT_TYPE = "text/plain; version=0.0.4; charset=utf-8";

type Labels = Record<string, string>;

interface Family {
	name: string;
	help: string;
	samples: { labels: Labels; value: number }[];
}

function escapeLabelValue(value: string): string {
	return value.replaceAll("\\", "\\\\").replaceAll('"', '\\"').replaceAll("\n", "\\n");
}

function formatFamily({ name, help, samples }: Family): string {
	const lines = [`# HELP ${name} ${help}`, `# TYPE ${name} gauge`];
	for (const { labels, value } of samples) {
		const rendered = Object.entries(labels)
			.map(([key, labelValue]) => `${key}="${escapeLabelValue(labelValue)}"`)
			.join(",");
		lines.push(`${name}{${rendered}} ${value}`);
	}
	return lines.join("\n");
}

export function formatQuotaMetrics(results: readonly AccountQuotaResult[]): string {
	const up: Family = {
		name: "cliproxy_quota_up",
		help: "Whether the account's last quota request succeeded.",
		samples: [],
	};
	const ready: Family = {
		name: "cliproxy_quota_ready",
		help: "Whether the provider currently accepts requests for the account.",
		samples: [],
	};
	const used: Family = {
		name: "cliproxy_quota_used_percent",
		help: "Percentage of the account's quota window already used.",
		samples: [],
	};
	const reset: Family = {
		name: "cliproxy_quota_reset_timestamp_seconds",
		help: "Unix time at which the account's quota window resets.",
		samples: [],
	};

	for (const { provider, account, quota } of results) {
		const accountLabels = { provider, account };
		up.samples.push({ labels: accountLabels, value: quota === null ? 0 : 1 });
		if (quota === null) continue;
		ready.samples.push({ labels: accountLabels, value: quota.ready ? 1 : 0 });

		const seen = new Set<string>();
		for (const window of quota.windows) {
			if (seen.has(window.label)) continue;
			seen.add(window.label);
			const windowLabels = { ...accountLabels, window: window.label };
			used.samples.push({ labels: windowLabels, value: window.usedPercent });
			if (window.resetAtMs !== null) {
				reset.samples.push({ labels: windowLabels, value: Math.round(window.resetAtMs / 1_000) });
			}
		}
	}

	return `${[up, ready, used, reset].map(formatFamily).join("\n")}\n`;
}
