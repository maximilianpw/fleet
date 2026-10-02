/**
 * HTTP client for `/quota/v1/{provider}`. Used by `cliproxyapi-util` on every
 * host; it never reads provider credentials itself.
 *
 * Settings come from the environment, then `~/.config/cliproxyapi/client.json`:
 *
 *   CLIPROXYAPI_QUOTA_URL     / quotaUrl     quota endpoint base, ending in /quota/v1
 *   CLIPROXYAPI_ROOT_URL      / rootUrl      fallback: `${rootUrl}/quota/v1` (non-loopback only)
 *   CLIPROXYAPI_API_KEY       / -            bearer token
 *   CLIPROXYAPI_API_KEY_FILE  / apiKeyFile   file containing the bearer token
 *
 * A loopback quota URL is the server host's direct access and needs no key.
 * Any other URL requires one.
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { DEFAULT_REQUEST_TIMEOUT_MS, requestSignal, type Fetch } from "./credentials.ts";
import { isRecord, parseProviderQuota, type ProviderQuota, type QuotaProvider } from "./providers.ts";

type ReadTextFile = (path: string) => string;

export type QuotaClientEnvironment = Readonly<Record<string, string | undefined>>;

export interface QuotaConnection {
	quotaUrl: string;
	apiKey: string | null;
}

export interface ResolveQuotaConnectionOptions {
	/** `null` disables the config file. Defaults to `~/.config/cliproxyapi/client.json`. */
	configFilePath?: string | null;
	readTextFile?: ReadTextFile;
}

function nonEmpty(value: string | undefined): string | undefined {
	const trimmed = value?.trim();
	return trimmed ? trimmed : undefined;
}

function readConfigFile(path: string | null, readTextFile: ReadTextFile): QuotaClientEnvironment {
	if (path === null) return {};
	let payload: unknown;
	try {
		payload = JSON.parse(readTextFile(path));
	} catch (error) {
		if (isRecord(error) && error.code === "ENOENT") return {};
		throw error;
	}
	if (!isRecord(payload)) {
		throw new Error(`${path} must contain a JSON object`);
	}
	const text = (value: unknown) => (typeof value === "string" ? value : undefined);
	return {
		CLIPROXYAPI_QUOTA_URL: text(payload.quotaUrl),
		CLIPROXYAPI_ROOT_URL: text(payload.rootUrl),
		CLIPROXYAPI_API_KEY_FILE: text(payload.apiKeyFile),
	};
}

export function isLoopbackUrl(url: string): boolean {
	const hostname = new URL(url).hostname;
	return hostname === "localhost" || hostname === "[::1]" || /^127\.\d+\.\d+\.\d+$/.test(hostname);
}

export function resolveQuotaConnection(
	environment: QuotaClientEnvironment = process.env,
	options: ResolveQuotaConnectionOptions = {},
): QuotaConnection {
	const readTextFile = options.readTextFile ?? ((path) => readFileSync(path, "utf8"));
	const file = readConfigFile(
		options.configFilePath === undefined
			? join(homedir(), ".config", "cliproxyapi", "client.json")
			: options.configFilePath,
		readTextFile,
	);

	const explicitQuotaUrl = nonEmpty(environment.CLIPROXYAPI_QUOTA_URL ?? file.CLIPROXYAPI_QUOTA_URL);
	const rootUrl = nonEmpty(environment.CLIPROXYAPI_ROOT_URL ?? file.CLIPROXYAPI_ROOT_URL);
	let quotaUrl: string;
	if (explicitQuotaUrl !== undefined) {
		quotaUrl = explicitQuotaUrl;
	} else if (rootUrl !== undefined && !isLoopbackUrl(rootUrl)) {
		quotaUrl = `${rootUrl.replace(/\/$/, "")}/quota/v1`;
	} else {
		throw new Error(
			rootUrl === undefined
				? "quota client requires CLIPROXYAPI_QUOTA_URL or a public CLIPROXYAPI_ROOT_URL"
				: "a loopback CLIPROXYAPI_ROOT_URL is the proxy, not the quota service; set CLIPROXYAPI_QUOTA_URL",
		);
	}
	quotaUrl = quotaUrl.replace(/\/$/, "");

	const directKey = nonEmpty(environment.CLIPROXYAPI_API_KEY);
	const keyFile = nonEmpty(environment.CLIPROXYAPI_API_KEY_FILE ?? file.CLIPROXYAPI_API_KEY_FILE);
	const apiKey = directKey ?? (keyFile === undefined ? undefined : nonEmpty(readTextFile(keyFile)));
	if (apiKey === undefined && !isLoopbackUrl(quotaUrl)) {
		throw new Error(`quota URL ${quotaUrl} is not loopback and requires CLIPROXYAPI_API_KEY or CLIPROXYAPI_API_KEY_FILE`);
	}
	return { quotaUrl, apiKey: apiKey ?? null };
}

export interface QuotaClient {
	/** `null` when the server has no credential for the provider (HTTP 404). */
	getQuota(provider: QuotaProvider, options?: { signal?: AbortSignal }): Promise<ProviderQuota | null>;
}

export function createQuotaClient(
	connection: QuotaConnection,
	fetchImplementation: Fetch = globalThis.fetch,
	requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
): QuotaClient {
	return {
		async getQuota(provider, options = {}) {
			const response = await fetchImplementation(`${connection.quotaUrl}/${provider}`, {
				headers:
					connection.apiKey === null ? {} : { authorization: `Bearer ${connection.apiKey}` },
				signal: requestSignal(options.signal, requestTimeoutMs),
			});
			if (response.status === 404) return null;
			if (!response.ok) {
				throw new Error(`${provider} quota request failed with HTTP ${response.status}`);
			}
			return parseProviderQuota(await response.json(), provider);
		},
	};
}
