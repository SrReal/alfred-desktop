import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFileSync, writeFileSync, renameSync } from "node:fs";
import { join } from "node:path";
import {
  McpServer,
  createMcpHandler,
  fromJsonSchema,
} from "@modelcontextprotocol/server";
import { serveStdio } from "@modelcontextprotocol/server/stdio";
import { toNodeHandler } from "@modelcontextprotocol/node";

const pins = Object.fromEntries(
  ["server", "node"].map((name) => [
    name,
    JSON.parse(
      readFileSync(
        new URL(
          `../../node_modules/@modelcontextprotocol/${name}/package.json`,
          import.meta.url,
        ),
        "utf8",
      ),
    ).version,
  ]),
);
assert.deepEqual(pins, { server: "2.3.0", node: "2.1.1" });
const empty = process.argv.includes("--empty");
const mailbox = process.argv.includes("--stdio") ? process.argv.at(-1) : null;
const uri = "owned:///one";
let instance,
  handle,
  tick,
  sequence = 0,
  acceptedCalls = 0,
  listens = 0;
const active = new Set(),
  cancelled = [],
  methods = [],
  errors = [];
const headers = [];
function factory() {
  instance = new McpServer({
    name: "alfredpet-owned-listen-sdk",
    version: "1.0.0",
  });
  if (!empty) {
    instance.registerTool(
      "owned_echo",
      {
        inputSchema: fromJsonSchema({
          type: "object",
          properties: { text: { type: "string" } },
          required: ["text"],
          additionalProperties: false,
        }),
      },
      async ({ text }) => {
        acceptedCalls++;
        return { content: [{ type: "text", text }] };
      },
    );
    instance.registerResource("owned", uri, {}, async (url) => ({
      contents: [{ uri: url.href, text: "owned" }],
    }));
    instance.server.registerCapabilities({
      tools: { listChanged: true },
      prompts: { listChanged: true },
      resources: { listChanged: true, subscribe: true },
    });
  }
  return instance;
}
function stats() {
  return {
    scope: "owned-listen-sdk-fixture",
    pins,
    sequence,
    acceptedCalls,
    listens,
    cancelled,
    methods,
    headers,
    errors,
    active: mailbox ? active.size : handle.bus.listenerCount,
  };
}
function save() {
  if (!mailbox) return;
  const target = join(mailbox, "stats.json");
  writeFileSync(target + ".pending", JSON.stringify(stats()));
  renameSync(target + ".pending", target);
}
async function command(value) {
  if (value.mode === "emit") {
    for (let n = 0; n < (value.count ?? 1); n++) {
      assert.ok(n < 140);
      if (mailbox) {
        await instance.server.sendToolListChanged();
        await instance.server.sendPromptListChanged();
        await instance.server.sendResourceListChanged();
        await instance.server.sendResourceUpdated({
          uri: "owned:///not-requested",
        });
        await instance.server.sendResourceUpdated({ uri });
      } else {
        handle.notify.toolsChanged();
        handle.notify.promptsChanged();
        handle.notify.resourcesChanged();
        handle.notify.resourceUpdated("owned:///not-requested");
        handle.notify.resourceUpdated(uri);
      }
    }
  } else if (value.mode === "close") {
    clearInterval(tick);
    await handle.close();
    active.clear();
  } else throw new Error("owned fixture command");
  sequence = value.sequence;
  save();
}
if (mailbox) {
  assert.ok(mailbox && !mailbox.startsWith("--"));
  handle = serveStdio(factory, {
    legacy: "reject",
    maxSubscriptions: 4,
    onerror: (e) => {
      errors.push(e.message);
      save();
    },
  });
  let pending = "",
    running = false;
  process.stdin.on("data", (chunk) => {
    pending += chunk.toString("utf8");
    for (;;) {
      const at = pending.indexOf("\n");
      if (at < 0) break;
      const line = pending.slice(0, at);
      pending = pending.slice(at + 1);
      const value = JSON.parse(line);
      methods.push(value.method);
      if (value.method === "subscriptions/listen") {
        active.add(value.id);
        listens++;
      }
      if (value.method === "notifications/cancelled") {
        cancelled.push(value.params.requestId);
        active.delete(value.params.requestId);
      }
    }
    save();
  });
  process.stdin.once("end", () => {
    clearInterval(tick);
    active.clear();
    save();
  });
  tick = setInterval(async () => {
    if (running) return;
    running = true;
    try {
      let value;
      try {
        value = JSON.parse(readFileSync(join(mailbox, "command.json"), "utf8"));
      } catch (e) {
        if (e.code !== "ENOENT") throw e;
      }
      if (value && value.sequence > sequence) await command(value);
    } finally {
      running = false;
    }
  }, 20);
  save();
} else {
  handle = createMcpHandler(factory, {
    legacy: "reject",
    keepAliveMs: 0,
    onerror: (e) => errors.push(e.message),
  });
  const mcp = toNodeHandler(handle);
  const server = createServer(async (req, res) => {
    if (req.url === "/mcp") {
      methods.push(req.method);
      headers.push({
        method: req.headers["mcp-method"],
        session: req.headers["mcp-session-id"] ?? null,
        lastEvent: req.headers["last-event-id"] ?? null,
      });
      if (req.headers["mcp-method"] === "subscriptions/listen") listens++;
      return void mcp(req, res);
    }
    if (req.method === "POST" && req.url === "/__owned_command") {
      let body = "";
      for await (const chunk of req) {
        body += chunk;
        assert.ok(body.length < 1024);
      }
      await command(JSON.parse(body));
    } else if (req.url !== "/__owned_stats") {
      res.writeHead(404);
      res.end();
      return;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(stats()));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  process.stdout.write(
    JSON.stringify({
      endpoint: `http://127.0.0.1:${server.address().port}/mcp`,
      scope: "owned-listen-sdk-fixture",
      pins,
    }) + "\n",
  );
}
