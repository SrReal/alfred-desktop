import readline from "node:readline";
const mode = process.argv[2] ?? "ok";
let initialized = false;
for await (const line of readline.createInterface({ input: process.stdin })) {
  const message = JSON.parse(line);
  if (message.method === "notifications/initialized") {
    initialized = true;
    continue;
  }
  if (mode === "malformed") {
    process.stdout.write("{broken\n");
    continue;
  }
  const result =
    message.method === "initialize"
      ? {
          protocolVersion: mode === "old" ? "1900-01-01" : "2025-11-25",
          capabilities: { tools: {}, resources: {}, prompts: {} },
          serverInfo: { name: "AlfredPet contract fixture", version: "1" },
        }
      : { tools: [{ name: "fixture", inputSchema: { type: "object" } }] };
  if (message.method !== "initialize" && !initialized) {
    process.stdout.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: message.id,
        error: { code: -32000, message: "initialized notification required" },
      }) + "\n",
    );
  } else {
    process.stdout.write(
      JSON.stringify({ jsonrpc: "2.0", id: message.id, result }) + "\n",
    );
  }
}
