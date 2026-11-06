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
export const DEFAULT_CACHE_TTL_MS = 60_000;
export const DEFAULT_MAX_STALE_MS = 30 * 60_000;
export const DEFAULT_REQUEST_TIMEOUT_MS = 8_000;
const FIRST_RETRY_DELAY_MS = 30_000;
const MAX_BACKOFF_MS = 15 * 60_000;
const MAX_RETRY_AFTER_MS = 60 * 60_000;

/** The subset of `fetch` this service uses; injectable for tests. */
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

interface StoredCredential {
	provider: QuotaProvider;
	account: string;
	accessToken: string;
	accountId: string | null;
}

export interface QuotaReaderOptions {
	credentialDirectory: string;
	cacheTtlMs?: number;
	maxStaleMs?: number;
	requestTimeoutMs?: number;
	fetch?: Fetch;
	now?: () => number;
	log?: (message: string) => void;
}

export interface GetQuotaOptions {
	signal?: AbortSignal;
}

export interface FetchedAccountQuota extends AccountQuota {
	fetchedAtMs: number;
}

export interface AccountQuotaResult {
	provider: QuotaProvider;
	account: string;
	lastFetchFailed: boolean;
	quota: FetchedAccountQuota | null;
}

export interface QuotaReader {
	/** `null` when no enabled credential exists for the provider. */
	getQuota(provider: QuotaProvider, options?: GetQuotaOptions): Promise<ProviderQuota | null>;
	getAccountQuotas(options?: GetQuotaOptions): Promise<AccountQuotaResult[]>;
}

class QuotaFetchError extends Error {
	constructor(
		readonly status: number,
		readonly retryAfter: string | null,
	) {
		super(`quota request failed with HTTP ${status}`);
		this.name = "QuotaFetchError";
	}
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
						account: name.slice(0, -".json".length),
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
		throw new QuotaFetchError(response.status, response.headers.get("retry-after"));
	}
	const payload: unknown = await response.json();
	if (credential.provider === "claude") return parseClaudeQuota(payload);
	return credential.provider === "codex" ? parseCodexQuota(payload) : parseXAIQuota(payload);
}

function parseRetryAfter(value: string | null, nowMs: number): number | null {
	if (value === null) return null;
	const trimmed = value.trim();
	if (/^\d+$/.test(trimmed)) return Number(trimmed) * 1_000;
	const dateMs = Date.parse(trimmed);
	return Number.isNaN(dateMs) ? null : Math.max(0, dateMs - nowMs);
}

function retryDelayMs(error: unknown, consecutiveFailures: number, nowMs: number): number {
	const backoff = Math.min(MAX_BACKOFF_MS, FIRST_RETRY_DELAY_MS * 2 ** (consecutiveFailures - 1));
	const retryAfter = error instanceof QuotaFetchError ? (parseRetryAfter(error.retryAfter, nowMs) ?? 0) : 0;
	return Math.min(MAX_RETRY_AFTER_MS, Math.max(backoff, retryAfter));
}

function describeFailure(error: unknown): string {
	if (error instanceof QuotaFetchError) return `HTTP ${error.status}`;
	if (error instanceof Error) return error.name === "TimeoutError" ? "timed out" : error.message;
	return String(error);
}

function untilAborted<T>(promise: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
	if (signal === undefined) return promise;
	signal.throwIfAborted();
	return new Promise((resolve, reject) => {
		const onAbort = () => reject(signal.reason);
		signal.addEventListener("abort", onAbort, { once: true });
		promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
	});
}

function highestUsage(quota: AccountQuota): number {
	return Math.max(...quota.windows.map((window) => window.usedPercent));
}

/** Report the least-used ready account, or the least-used account if none is ready. */
function selectAccountQuota(quotas: readonly FetchedAccountQuota[]): FetchedAccountQuota {
	const ready = quotas.filter((quota) => quota.ready);
	const candidates = ready.length > 0 ? ready : quotas;
	const selected = [...candidates].sort((left, right) => highestUsage(left) - highestUsage(right))[0];
	if (selected === undefined) {
		throw new Error("quota selection requires at least one account");
	}
	return selected;
}

interface AccountState {
	accessToken: string;
	latest: FetchedAccountQuota | null;
	lastFetchFailed: boolean;
	consecutiveFailures: number;
	retryAtMs: number;
	inFlight: Promise<void> | null;
}

export function createQuotaReader(options: QuotaReaderOptions): QuotaReader {
	const cacheTtlMs = options.cacheTtlMs ?? DEFAULT_CACHE_TTL_MS;
	const maxStaleMs = options.maxStaleMs ?? DEFAULT_MAX_STALE_MS;
	const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
	const fetchImplementation = options.fetch ?? globalThis.fetch;
	const now = options.now ?? Date.now;
	const log = options.log ?? ((message: string) => console.error(message));
	const states = new Map<string, AccountState>();

	function stateFor(credential: StoredCredential): AccountState {
		const state = states.get(credential.account);
		if (state === undefined) {
			const created: AccountState = {
				accessToken: credential.accessToken,
				latest: null,
				lastFetchFailed: false,
				consecutiveFailures: 0,
				retryAtMs: 0,
				inFlight: null,
			};
			states.set(credential.account, created);
			return created;
		}
		if (state.accessToken !== credential.accessToken) {
			// CLIProxyAPI refreshed the token; the backoff may have been caused by the old one.
			state.accessToken = credential.accessToken;
			state.retryAtMs = 0;
		}
		return state;
	}

	async function refresh(credential: StoredCredential, state: AccountState): Promise<void> {
		try {
			const quota = await fetchAccountQuota(fetchImplementation, credential, AbortSignal.timeout(requestTimeoutMs));
			state.latest = { ...quota, fetchedAtMs: now() };
			state.lastFetchFailed = false;
			state.consecutiveFailures = 0;
			state.retryAtMs = 0;
		} catch (error) {
			state.lastFetchFailed = true;
			state.consecutiveFailures += 1;
			const delayMs = retryDelayMs(error, state.consecutiveFailures, now());
			state.retryAtMs = now() + delayMs;
			log(
				`cliproxy-quota: ${credential.provider} account ${credential.account}: ${describeFailure(error)}; next request in ${Math.round(delayMs / 1_000)}s`,
			);
		} finally {
			state.inFlight = null;
		}
	}

	async function accountQuota(
		credential: StoredCredential,
		signal: AbortSignal | undefined,
	): Promise<AccountQuotaResult> {
		const state = stateFor(credential);
		const fresh = state.latest !== null && now() - state.latest.fetchedAtMs < cacheTtlMs;
		if (!fresh && state.inFlight === null && now() >= state.retryAtMs) {
			state.inFlight = refresh(credential, state);
		}
		if (state.inFlight !== null) await untilAborted(state.inFlight, signal);
		const usable = state.latest !== null && now() - state.latest.fetchedAtMs < maxStaleMs ? state.latest : null;
		return {
			provider: credential.provider,
			account: credential.account,
			lastFetchFailed: state.lastFetchFailed,
			quota: usable,
		};
	}

	return {
		async getQuota(provider, requestOptions = {}) {
			const credentials = (await readStoredCredentials(options.credentialDirectory)).filter(
				(credential) => credential.provider === provider,
			);
			if (credentials.length === 0) return null;

			const results = await Promise.all(
				credentials.map((credential) => accountQuota(credential, requestOptions.signal)),
			);
			const quotas = results.flatMap((result) => (result.quota === null ? [] : [result.quota]));
			if (quotas.length === 0) {
				throw new Error(`${provider} quota request failed for every account`);
			}

			const selected = selectAccountQuota(quotas);
			return {
				provider,
				windows: selected.windows,
				readyAccounts: quotas.filter((account) => account.ready).length,
				totalAccounts: credentials.length,
				fetchedAtMs: selected.fetchedAtMs,
			};
		},

		async getAccountQuotas(requestOptions = {}) {
			const credentials = await readStoredCredentials(options.credentialDirectory);
			return Promise.all(credentials.map((credential) => accountQuota(credential, requestOptions.signal)));
		},
	};
}
