/**
 * Server-side quota reader. Reads CLIProxyAPI's provider credential files and
 * asks each provider for that account's usage. Only parsed summaries leave
 * this module; tokens never do.
 */
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import {
	isQuotaProvider,
	isRecord,
	parseClaudeQuota,
	parseCodexQuota,
	parseXAIQuota,
	type AccountQuota,
	type ProviderQuota,
	type QuotaProvider,
} from "./providers.ts";

export const CLAUDE_USAGE_URL = "https://api.anthropic.com/api/oauth/usage";
export const CODEX_USAGE_URL = "https://chatgpt.com/backend-api/wham/usage";
export const XAI_USAGE_URL = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
export const DEFAULT_CACHE_TTL_MS = 15_000;
export const DEFAULT_REQUEST_TIMEOUT_MS = 8_000;

/** The subset of `fetch` this service uses; injectable for tests. */
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

interface StoredCredential {
	provider: QuotaProvider;
	accessToken: string;
	accountId: string | null;
}

export interface QuotaReaderOptions {
	credentialDirectory: string;
	cacheTtlMs?: number;
	requestTimeoutMs?: number;
	fetch?: Fetch;
	now?: () => number;
}

export interface GetQuotaOptions {
	force?: boolean;
	signal?: AbortSignal;
}

export interface QuotaReader {
	/** `null` when no enabled credential exists for the provider. */
	getQuota(provider: QuotaProvider, options?: GetQuotaOptions): Promise<ProviderQuota | null>;
}

async function readStoredCredentials(directory: string): Promise<StoredCredential[]> {
	let names: string[];
	try {
		names = await readdir(directory);
	} catch (error) {
		if (isRecord(error) && error.code === "ENOENT") return [];
		throw error;
	}

	const records = await Promise.all(
		names
			.filter((name) => name.endsWith(".json"))
			.map(async (name): Promise<StoredCredential | null> => {
				try {
					const payload: unknown = JSON.parse(await readFile(join(directory, name), "utf8"));
					if (!isRecord(payload) || payload.disabled === true) return null;
					if (!isQuotaProvider(payload.type)) return null;
					if (typeof payload.access_token !== "string" || payload.access_token.length === 0) {
						return null;
					}
					return {
						provider: payload.type,
						accessToken: payload.access_token,
						accountId:
							typeof payload.account_id === "string" && payload.account_id.length > 0
								? payload.account_id
								: null,
					};
				} catch {
					return null;
				}
			}),
	);
	return records.filter((record): record is StoredCredential => record !== null);
}

export function requestSignal(signal: AbortSignal | undefined, timeoutMs: number): AbortSignal {
	const timeoutSignal = AbortSignal.timeout(timeoutMs);
	return signal === undefined ? timeoutSignal : AbortSignal.any([signal, timeoutSignal]);
}

async function fetchAccountQuota(
	fetchImplementation: Fetch,
	credential: StoredCredential,
	signal: AbortSignal,
): Promise<AccountQuota> {
	const headers: Record<string, string> = {
		authorization: `Bearer ${credential.accessToken}`,
	};
	let url: string;
	if (credential.provider === "claude") {
		url = CLAUDE_USAGE_URL;
		headers["anthropic-beta"] = "oauth-2025-04-20";
		headers["content-type"] = "application/json";
	} else if (credential.provider === "codex") {
		url = CODEX_USAGE_URL;
		headers["content-type"] = "application/json";
		headers["user-agent"] = "codex-tui/0.149.1";
		if (credential.accountId !== null) headers["chatgpt-account-id"] = credential.accountId;
	} else {
		url = XAI_USAGE_URL;
		headers.accept = "*/*";
		headers["user-agent"] = "grok-pager/0.2.91 grok-shell/0.2.91";
		headers["x-grok-client-version"] = "0.2.91";
		headers["x-xai-token-auth"] = "xai-grok-cli";
	}

	const response = await fetchImplementation(url, { headers, signal });
	if (!response.ok) {
		throw new Error(`${credential.provider} quota request failed with HTTP ${response.status}`);
	}
	const payload: unknown = await response.json();
	if (credential.provider === "claude") return parseClaudeQuota(payload);
	return credential.provider === "codex" ? parseCodexQuota(payload) : parseXAIQuota(payload);
}

function highestUsage(quota: AccountQuota): number {
	return Math.max(...quota.windows.map((window) => window.usedPercent));
}

/** Report the least-used ready account, or the least-used account if none is ready. */
function selectAccountQuota(quotas: readonly AccountQuota[]): AccountQuota {
	const ready = quotas.filter((quota) => quota.ready);
	const candidates = ready.length > 0 ? ready : quotas;
	const selected = [...candidates].sort((left, right) => highestUsage(left) - highestUsage(right))[0];
	if (selected === undefined) {
		throw new Error("quota selection requires at least one account");
	}
	return selected;
}

export function createQuotaReader(options: QuotaReaderOptions): QuotaReader {
	const cacheTtlMs = options.cacheTtlMs ?? DEFAULT_CACHE_TTL_MS;
	const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
	const fetchImplementation = options.fetch ?? globalThis.fetch;
	const now = options.now ?? Date.now;
	const cache = new Map<QuotaProvider, ProviderQuota>();

	return {
		async getQuota(provider, requestOptions = {}) {
			const cached = cache.get(provider);
			if (
				requestOptions.force !== true &&
				cached !== undefined &&
				now() - cached.fetchedAtMs < cacheTtlMs
			) {
				return cached;
			}

			const credentials = (await readStoredCredentials(options.credentialDirectory)).filter(
				(credential) => credential.provider === provider,
			);
			if (credentials.length === 0) return null;

			const signal = requestSignal(requestOptions.signal, requestTimeoutMs);
			const results = await Promise.allSettled(
				credentials.map((credential) => fetchAccountQuota(fetchImplementation, credential, signal)),
			);
			const quotas = results.flatMap((result) =>
				result.status === "fulfilled" ? [result.value] : [],
			);
			if (quotas.length === 0) {
				throw new Error(`${provider} quota request failed for every account`);
			}

			const selected = selectAccountQuota(quotas);
			const quota: ProviderQuota = {
				provider,
				windows: selected.windows,
				readyAccounts: quotas.filter((account) => account.ready).length,
				totalAccounts: credentials.length,
				fetchedAtMs: now(),
			};
			cache.set(provider, quota);
			return quota;
		},
	};
}
