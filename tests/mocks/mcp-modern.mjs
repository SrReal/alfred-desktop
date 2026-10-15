import readline from "node:readline";
import { appendFileSync } from "node:fs";

const mode = process.argv[2] ?? "modern";
const log = process.argv[3];
let initialized = false;
const write = (id, result) =>
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
for await (const line of readline.createInterface({ input: process.stdin })) {
  const message = JSON.parse(line);
  if (log)
    appendFileSync(
      log,
      JSON.stringify({
        pid: process.pid,
        method: message.method,
        requestId: message.params?.requestId,
      }) + "\n",
    );
  if (message.method === "notifications/cancelled") continue;
  if (mode === "partial-frame") {
    process.stdout.write(
      JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {} }),
    );
    process.exit(0);
  }
  if (mode === "server-request") {
    process.stdout.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: "unrequested",
        method: "sampling/createMessage",
        params: {},
      }) + "\n",
    );
    continue;
  }
  if (mode === "notification-flood") {
    process.stdout.write(
      (
        JSON.stringify({
          jsonrpc: "2.0",
          method: "notifications/tools/list_changed",
        }) + "\n"
      ).repeat(257),
    );
    continue;
  }
  if (message.method === "server/discover") {
    if (mode === "timeout-legacy") continue;
    if (mode === "dual-legacy") {
      process.stdout.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32601, message: "unknown method" },
        }) + "\n",
      );
      continue;
    }
    write(message.id, {
      resultType: "complete",
      supportedVersions: ["2026-07-28"],
      capabilities: { tools: {}, resources: {}, prompts: {} },
    });
    continue;
  }
  if (message.method === "initialize") {
    write(message.id, {
      protocolVersion: "2025-11-25",
      capabilities: { tools: {} },
      serverInfo: { name: "owned fixture", version: "1" },
    });
    continue;
  }
  if (message.method === "notifications/initialized") {
    initialized = true;
    continue;
  }
  const legacy = ["dual-legacy", "timeout-legacy"].includes(mode);
  if (legacy && !initialized) throw new Error("legacy notification missing");
  if (
    !legacy &&
    message.params._meta["io.modelcontextprotocol/protocolVersion"] !==
      "2026-07-28"
  )
    throw new Error("modern metadata missing");
  if (message.method === "tools/call" && mode === "hang-call") continue;
  const result =
    message.method === "tools/list"
      ? {
          tools: [{ name: "fixture", inputSchema: { type: "object" } }],
        }
      : { content: [], structuredContent: { ok: true } };
  if (!legacy) result.resultType = "complete";
  write(message.id, result);
}
