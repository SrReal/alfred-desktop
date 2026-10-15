"use strict";
const crypto = require("node:crypto");
const path = require("node:path");
const fs = require("node:fs/promises");
const LIMIT = 1024 * 1024;
function fail(code) {
  throw new Error(code);
}
function object(v) {
  return !!v && !Array.isArray(v) && typeof v === "object";
}
function keys(v, allowed, required = allowed) {
  if (
    !object(v) ||
    Object.keys(v).some((k) => !allowed.includes(k)) ||
    required.some((k) => !Object.hasOwn(v, k))
  )
    fail("VSCODE_INPUT");
}
function text(v, max = 8192) {
  return (
    typeof v === "string" &&
    v.length > 0 &&
    v.length <= max &&
    !/[\x00-\x1f]/.test(v)
  );
}
function uuid(v) {
  return (
    typeof v === "string" &&
    /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(v)
  );
}
function secret(v) {
  return /(?:sk-ant-|sk-proj-|gh[pousr]_|github_pat_|Bearer\s+[a-z0-9_.-]{12,}|(?:password|secret|token|api[_-]?key)["']?\s*[=:]\s*["']?[^\s"']{6,})/i.test(
    v,
  );
}
function canonical(v, depth = 0, budget = { n: 0 }) {
  if (depth > 32 || ++budget.n > 10000) fail("VSCODE_VALUE_LIMIT");
  if (v === null || typeof v === "boolean" || typeof v === "string") {
    if (typeof v === "string" && v.length > 64000) fail("VSCODE_VALUE_LIMIT");
    return v;
  }
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (Array.isArray(v)) return v.map((x) => canonical(x, depth + 1, budget));
  if (
    !object(v) ||
    ![Object.prototype, null].includes(Object.getPrototypeOf(v))
  )
    fail("VSCODE_VALUE_TYPE");
  const out = Object.create(null);
  for (const k of Object.keys(v).sort()) {
    if (["__proto__", "constructor", "prototype"].includes(k) || k.length > 200)
      fail("VSCODE_VALUE_KEY");
    out[k] = canonical(v[k], depth + 1, budget);
  }
  return out;
}
function hash(v) {
  return crypto
    .createHash("sha256")
    .update(typeof v === "string" ? v : JSON.stringify(canonical(v)))
    .digest("hex");
}
// Bounded inert JSONC: no expression/variable evaluation; duplicate keys denied.
function jsonc(source) {
  if (typeof source !== "string" || Buffer.byteLength(source) > 256 * 1024)
    fail("VSCODE_CONFIG_LIMIT");
  let i = source.charCodeAt(0) === 0xfeff ? 1 : 0,
    nodes = 0;
  function ws() {
    for (;;) {
      while (/[ \t\r\n]/.test(source[i] || "X")) i++;
      if (source.slice(i, i + 2) === "//") {
        i += 2;
        while (i < source.length && !/[\r\n]/.test(source[i])) i++;
      } else if (source.slice(i, i + 2) === "/*") {
        const end = source.indexOf("*/", i + 2);
        if (end < 0) fail("VSCODE_CONFIG_JSONC");
        i = end + 2;
      } else return;
    }
  }
  function string() {
    const start = i++;
    while (i < source.length) {
      const c = source[i++];
      if (c === "\\") i++;
      else if (c === '"') {
        try {
          return JSON.parse(source.slice(start, i));
        } catch {
          fail("VSCODE_CONFIG_JSONC");
        }
      }
    }
    fail("VSCODE_CONFIG_JSONC");
  }
  function value(depth) {
    if (depth > 32 || ++nodes > 10000) fail("VSCODE_CONFIG_LIMIT");
    ws();
    const c = source[i];
    if (c === '"') return string();
    if (c === "{" || c === "[") {
      const dict = c === "{",
        out = dict ? Object.create(null) : [],
        end = dict ? "}" : "]";
      i++;
      ws();
      if (source[i] === end) {
        i++;
        return out;
      }
      for (;;) {
        ws();
        let key;
        if (dict) {
          if (source[i] !== '"') fail("VSCODE_CONFIG_JSONC");
          key = string();
          ws();
          if (
            ["__proto__", "constructor", "prototype"].includes(key) ||
            Object.hasOwn(out, key)
          )
            fail("VSCODE_CONFIG_DUPLICATE_OR_KEY");
          if (source[i++] !== ":") fail("VSCODE_CONFIG_JSONC");
        }
        const next = value(depth + 1);
        if (dict) out[key] = next;
        else out.push(next);
        ws();
        if (source[i] === end) {
          i++;
          return out;
        }
        if (source[i++] !== ",") fail("VSCODE_CONFIG_JSONC");
        ws();
        if (source[i] === end) {
          i++;
          return out;
        }
      }
    }
    const match =
      /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
        source.slice(i),
      );
    if (!match) fail("VSCODE_CONFIG_JSONC");
    i += match[0].length;
    const result = JSON.parse(match[0]);
    if (typeof result === "number" && !Number.isFinite(result))
      fail("VSCODE_CONFIG_JSONC");
    return result;
  }
  const result = value(0);
  ws();
  if (i !== source.length) fail("VSCODE_CONFIG_JSONC");
  return result;
}
function normalized(p) {
  return path
    .resolve(p)
    .replace(/^\\\\\?\\/, "")
    .replace(/[\\/]$/, "");
}
function same(a, b) {
  return process.platform === "win32"
    ? normalized(a).toLowerCase() === normalized(b).toLowerCase()
    : normalized(a) === normalized(b);
}
async function localPath(raw, root, directory = false) {
  if (
    !text(raw) ||
    !path.isAbsolute(raw) ||
    /(?:^|[\\/])\.\.(?:[\\/]|$)/.test(raw)
  )
    fail("VSCODE_PATH");
  let current = path.resolve(raw);
  for (;;) {
    const stat = await fs.lstat(current);
    if (stat.isSymbolicLink()) fail("VSCODE_LINK_DENIED");
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  const resolved = await fs.realpath(raw),
    relative = path.relative(await fs.realpath(root), resolved);
  if (
    relative.startsWith(".." + path.sep) ||
    relative === ".." ||
    path.isAbsolute(relative) ||
    !same(resolved, raw)
  )
    fail("VSCODE_SCOPE");
  const stat = await fs.lstat(resolved);
  if (directory ? !stat.isDirectory() : !stat.isFile() || stat.nlink !== 1)
    fail("VSCODE_PATH_TYPE");
  return resolved;
}
function request(v) {
  keys(v, ["id", "session", "operation", "input"]);
  if (!uuid(v.id) || !uuid(v.session) || !text(v.operation, 40))
    fail("VSCODE_INPUT");
  const input = v.input;
  switch (v.operation) {
    case "info":
    case "commands":
    case "tasks":
    case "diagnostics":
    case "lease":
    case "revoke":
      keys(input, []);
      break;
    case "command":
      keys(input, ["command", "arguments", "fingerprint"]);
      if (
        !text(input.command, 200) ||
        input.command.startsWith("_") ||
        !Array.isArray(input.arguments) ||
        input.arguments.length > 20 ||
        input.arguments.some(
          (x) =>
            !["string", "number", "boolean"].includes(typeof x) && x !== null,
        ) ||
        secret(JSON.stringify(input.arguments))
      )
        fail("VSCODE_COMMAND_INPUT");
      break;
    case "run_task":
      keys(input, ["task", "fingerprint"]);
      if (!text(input.task, 64)) fail("VSCODE_TASK_INPUT");
      break;
    case "task_status":
    case "task_cancel":
      keys(input, ["run"]);
      if (!uuid(input.run)) fail("VSCODE_RUN_INPUT");
      break;
    case "open":
      keys(input, ["path", "line", "column"], ["path"]);
      if (
        !text(input.path) ||
        ["line", "column"].some(
          (k) =>
            Object.hasOwn(input, k) &&
            (!Number.isSafeInteger(input[k]) ||
              input[k] < 1 ||
              input[k] > 1000000),
        )
      )
        fail("VSCODE_OPEN_INPUT");
      break;
    default:
      fail("VSCODE_OPERATION");
  }
  if (
    ["command", "run_task"].includes(v.operation) &&
    !/^[a-f0-9]{64}$/.test(input.fingerprint)
  )
    fail("VSCODE_OBSERVATION");
  canonical(v);
  if (Buffer.byteLength(JSON.stringify(v)) > 64 * 1024)
    fail("VSCODE_INPUT_LIMIT");
  return v;
}
module.exports = {
  LIMIT,
  fail,
  object,
  keys,
  text,
  uuid,
  secret,
  canonical,
  hash,
  jsonc,
  normalized,
  same,
  localPath,
  request,
};
