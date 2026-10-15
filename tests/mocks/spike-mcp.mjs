import readline from "node:readline";
import { readFile, appendFile } from "node:fs/promises";
import { resolve } from "node:path";
const tools = [
  {
    name: "echo",
    description: "Echo harmless compatibility fixture",
    inputSchema: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
  },
  {
    name: "guarded_probe",
    description: "Harmless probe requiring permission; returns input text",
    inputSchema: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
  },
  {
    name: "request_permission",
    description: "Permission handler",
    inputSchema: {
      type: "object",
      properties: {
        tool_name: { type: "string" },
        input: { type: "object", additionalProperties: true },
      },
      required: ["tool_name", "input"],
    },
  },
];
for await (const line of readline.createInterface({ input: process.stdin })) {
  try {
    const req = JSON.parse(line);
    if (!("id" in req)) continue;
    let result = {};
    if (req.method === "initialize")
      result = {
        protocolVersion: req.params.protocolVersion,
        capabilities: { tools: {} },
        serverInfo: { name: "alfredpet", version: "0.1.0" },
      };
    else if (req.method === "tools/list") result = { tools };
    else if (req.method === "tools/call") {
      let data = req.params.arguments;
      if (req.params.name === "request_permission") {
        const decision = (
          await readFile(
            resolve(process.env.ALFREDPET_SPIKE_DIR, "decision.txt"),
            "utf8",
          )
        ).trim();
        await appendFile(
          resolve(process.env.ALFREDPET_SPIKE_DIR, "requests.txt"),
          decision + "\n",
        );
        data =
          decision === "allow"
            ? { behavior: "allow", updatedInput: req.params.arguments.input }
            : { behavior: "deny", message: "Denied by compatibility fixture" };
      }
      result = { content: [{ type: "text", text: JSON.stringify(data) }] };
    }
    process.stdout.write(
      JSON.stringify({ jsonrpc: "2.0", id: req.id, result }) + "\n",
    );
  } catch {
    process.stderr.write("Invalid MCP request\n");
  }
}
