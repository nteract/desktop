#!/usr/bin/env bun
/**
 * CI webhook channel for Claude Code.
 *
 * Listens on a local HTTP port and forwards CI events (build failures,
 * test results, workflow notifications) into the active Claude Code session.
 *
 * One-way channel: Claude receives events and investigates, no reply needed.
 *
 * Usage:
 *   claude --dangerously-load-development-channels server:ci
 *
 * Test with:
 *   curl -X POST localhost:8789 \
 *     -H "Content-Type: application/json" \
 *     -H "X-GitHub-Event: workflow_run" \
 *     -d '{"job":"lint","status":"failure","run_url":"https://github.com/nteract/desktop/actions/runs/123"}'
 */
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

const PORT = Number(process.env.CI_CHANNEL_PORT ?? "8789");

const mcp = new Server(
	{ name: "ci", version: "0.0.1" },
	{
		capabilities: { experimental: { "claude/channel": {} } },
		instructions: [
			'CI events arrive as <channel source="ci" event="..." method="POST">.',
			"Each event is a JSON payload from GitHub Actions or a similar CI system.",
			"When you receive a failure event:",
			"  1. Read the job name and run URL from the payload",
			"  2. Investigate the relevant source files and recent changes",
			"  3. Identify the root cause and suggest or apply a fix",
			"  4. If the failure is a lint/format issue, run `cargo xtask lint --fix`",
			"  5. If the failure is a test, read the test and the code under test",
			"This is a one-way channel — act on events, no reply expected.",
		].join("\n"),
	},
);

await mcp.connect(new StdioServerTransport());

const server = Bun.serve({
	port: PORT,
	hostname: "127.0.0.1",
	async fetch(req) {
		if (req.method !== "POST") {
			return new Response("POST only", { status: 405 });
		}

		const body = await req.text();
		const url = new URL(req.url);
		const event = req.headers.get("X-GitHub-Event") ?? "ci";

		await mcp.notification({
			method: "notifications/claude/channel",
			params: {
				content: body,
				meta: {
					event,
					path: url.pathname,
					method: req.method,
				},
			},
		});

		return new Response("ok");
	},
});

// Log to stderr so it doesn't interfere with MCP stdio transport
console.error(`ci-webhook channel listening on http://127.0.0.1:${server.port}`);
