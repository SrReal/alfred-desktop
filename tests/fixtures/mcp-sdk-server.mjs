import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { PassThrough } from "node:stream";
import { readFileSync } from "node:fs";
import {
  createMcpHandler,
  fromJsonSchema,
  McpServer,
} from "@modelcontextprotocol/server";
import {
  serveStdio,
  StdioServerTransport,
} from "@modelcontextprotocol/server/stdio";
import { toNodeHandler } from "@modelcontextprotocol/node";

const pins = {
  server: JSON.parse(
    readFileSync(
      new URL(
        "../../node_modules/@modelcontextprotocol/server/package.json",
        import.meta.url,
      ),
      "utf8",
    ),
  ).version,
  node: JSON.parse(
    readFileSync(
      new URL(
        "../../node_modules/@modelcontextprotocol/node/package.json",
        import.meta.url,
      ),
      "utf8",
    ),
  ).version,
};
assert.equal(pins.server, "2.3.0");
assert.equal(pins.node, "2.1.1");
let acceptedCalls = 0;

function factory() {
  const server = new McpServer({
    name: "alfredpet-owned-official-sdk",
    version: "1.0.0",
  });
  server.registerTool(
    "owned_echo",
    {
      description:
        "Owned SDK fixture: bounded text echo with no external effects.",
      inputSchema: fromJsonSchema({
        type: "object",
        properties: { text: { type: "string", maxLength: 128 } },
        required: ["text"],
        additionalProperties: false,
      }),
      outputSchema: fromJsonSchema({
        type: "object",
        properties: { echo: { type: "string" } },
        required: ["echo"],
        additionalProperties: false,
      }),
      annotations: { readOnlyHint: true },
    },
    async ({ text }) => {
      acceptedCalls += 1;
      return {
        content: [{ type: "text", text }],
        structuredContent: { echo: text },
      };
    },
  );
  return server;
}

if (process.argv.includes("--stdio")) {
  const delayed = process.argv.includes("--owned-delay-discovery")
    ? new PassThrough()
    : null;
  if (delayed) setTimeout(() => process.stdin.pipe(delayed), 4000);
  const handle = serveStdio(
    factory,
    delayed
      ? { transport: new StdioServerTransport(delayed, process.stdout) }
      : {},
  );
  process.once("SIGTERM", () => {
    void handle.close();
  });
  process.once("SIGINT", () => {
    void handle.close();
  });
} else {
  const handler = createMcpHandler(factory, { legacy: "stateless" });
  const mcp = toNodeHandler(handler);
  const server = createServer((request, response) => {
    if (request.method === "GET" && request.url === "/__owned_sdk_stats") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ acceptedCalls, pins }));
    } else if (request.url === "/mcp") {
      void mcp(request, response);
    } else {
      response.writeHead(404);
      response.end();
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  process.stdout.write(
    JSON.stringify({
      endpoint: `http://127.0.0.1:${server.address().port}/mcp`,
      scope: "owned-loopback-sdk-fixture",
      pins,
    }) + "\n",
  );
  let stopping = false;
  async function stop() {
    if (stopping) return;
    stopping = true;
    await handler.close();
    server.close();
    server.closeAllConnections();
  }
  process.once("SIGTERM", () => {
    void stop();
  });
  process.once("SIGINT", () => {
    void stop();
  });
}
