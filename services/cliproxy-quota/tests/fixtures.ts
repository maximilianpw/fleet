import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export function codexPayload(
	usedPercent: number,
	{ allowed = true, limitReached = false }: { allowed?: boolean; limitReached?: boolean } = {},
) {
	return {
		rate_limit: {
			allowed,
			limit_reached: limitReached,
			primary_window: { used_percent: usedPercent, limit_window_seconds: 604_800, reset_at: 1_788_454_137 },
			secondary_window: null,
		},
	};
}

/** A temporary credential directory, removed by the returned cleanup. */
export async function credentialDirectory(
	files: Record<string, unknown>,
): Promise<{ path: string; [Symbol.asyncDispose](): Promise<void> }> {
	const path = await mkdtemp(join(tmpdir(), "cliproxy-quota-"));
	await Promise.all(
		Object.entries(files).map(([name, body]) =>
			writeFile(join(path, name), typeof body === "string" ? body : JSON.stringify(body)),
		),
	);
	return { path, [Symbol.asyncDispose]: () => rm(path, { recursive: true, force: true }) };
}
