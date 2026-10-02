/**
 * Quota-only loopback HTTP surface for CLIProxyAPI credentials.
 *
 * Contract, unchanged from the pi-config implementation:
 *   GET /quota/v1/{claude,codex,xai}
 *     200 ProviderQuota JSON, `cache-control: no-store`
 *     404 unknown path, non-GET method, or no credential for the provider
 *     503 every upstream quota request failed or timed out
 *
 * There is no authentication here. The listener must stay on loopback; a
 * reverse proxy authenticates public clients before forwarding.
 */
import { isIP } from "node:net";
import { parseArgs } from "node:util";
import { createQuotaReader, type QuotaReader } from "./credentials.ts";
import { isQuotaProvider } from "./providers.ts";

const QUOTA_PATH = /^\/quota\/v1\/([a-z]+)$/;

export function createQuotaHandler(reader: QuotaReader): (request: Request) => Promise<Response> {
	return async (request) => {
		const match = QUOTA_PATH.exec(new URL(request.url).pathname);
		const provider = match?.[1];
		if (request.method !== "GET" || !isQuotaProvider(provider)) {
			return new Response(null, { status: 404 });
		}
		try {
			const quota = await reader.getQuota(provider, { signal: request.signal });
			if (quota === null) return new Response(null, { status: 404 });
			return Response.json(quota, { headers: { "cache-control": "no-store" } });
		} catch {
			return new Response(null, { status: 503 });
		}
	};
}

export function isLoopbackAddress(address: string): boolean {
	if (isIP(address) === 4) return address.startsWith("127.");
	return address === "::1";
}

export interface ServerOptions {
	credentialDirectory: string;
	listenAddress: string;
	port: number;
}

const USAGE = `Usage: cliproxy-quota-server --credential-dir DIR [--listen-address ADDR] [--port PORT]

Serve GET /quota/v1/{claude,codex,xai} from CLIProxyAPI credential files.

Options:
  --credential-dir DIR    CLIProxyAPI auth directory (required)
  --listen-address ADDR   Loopback IP literal (default 127.0.0.1)
  --port PORT             TCP port (default 8318)`;

export function parseServerArgs(args: readonly string[]): ServerOptions | { error: string } | "help" {
	let values;
	try {
		({ values } = parseArgs({
			args: [...args],
			options: {
				"credential-dir": { type: "string" },
				"listen-address": { type: "string", default: "127.0.0.1" },
				port: { type: "string", default: "8318" },
				help: { type: "boolean", short: "h" },
			},
			strict: true,
			allowPositionals: false,
		}));
	} catch (error) {
		return { error: error instanceof Error ? error.message : String(error) };
	}
	if (values.help) return "help";

	const credentialDirectory = values["credential-dir"];
	if (credentialDirectory === undefined || credentialDirectory.length === 0) {
		return { error: "--credential-dir is required" };
	}
	const listenAddress = values["listen-address"];
	if (!isLoopbackAddress(listenAddress)) {
		return { error: `--listen-address must be a loopback IP literal, got ${listenAddress}` };
	}
	const port = Number(values.port);
	if (!Number.isInteger(port) || port < 1 || port > 65_535) {
		return { error: `--port must be 1..65535, got ${values.port}` };
	}
	return { credentialDirectory, listenAddress, port };
}

export function startServer(options: ServerOptions): ReturnType<typeof Bun.serve> {
	const handle = createQuotaHandler(
		createQuotaReader({ credentialDirectory: options.credentialDirectory }),
	);
	return Bun.serve({
		hostname: options.listenAddress,
		port: options.port,
		fetch: handle,
	});
}

if (import.meta.main) {
	const parsed = parseServerArgs(Bun.argv.slice(2));
	if (parsed === "help") {
		console.log(USAGE);
	} else if ("error" in parsed) {
		console.error(`cliproxy-quota-server: ${parsed.error}\n\n${USAGE}`);
		process.exit(2);
	} else {
		const server = startServer(parsed);
		console.error(`cliproxy-quota-server: listening on ${server.url.href}`);
	}
}
