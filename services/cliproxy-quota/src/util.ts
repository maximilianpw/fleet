import { createQuotaClient, resolveQuotaConnection } from "./client.ts";
import { formatQuotaReport, getQuotaReport } from "./report.ts";

const USAGE = `Usage: cliproxyapi-util quota [--json]

Commands:
  quota    Report live Codex, Claude, and Grok quota availability

Options:
  --json   Emit stable machine-readable JSON`;

async function main(args: readonly string[]): Promise<number> {
	if (args.includes("--help") || args.includes("-h")) {
		console.log(USAGE);
		return 0;
	}
	const [command, ...options] = args;
	if (command !== "quota" || options.some((option) => option !== "--json")) {
		console.log(USAGE);
		return 2;
	}

	let connection;
	try {
		connection = resolveQuotaConnection();
	} catch (error) {
		console.error(`cliproxyapi-util: ${error instanceof Error ? error.message : String(error)}`);
		return 2;
	}
	const report = await getQuotaReport(createQuotaClient(connection));
	console.log(options.includes("--json") ? JSON.stringify(report, null, 2) : formatQuotaReport(report));
	return 0;
}

process.exitCode = await main(Bun.argv.slice(2));
