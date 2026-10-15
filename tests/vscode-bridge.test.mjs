import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { randomUUID } from "node:crypto";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  realpath,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
const require = createRequire(import.meta.url);
const P = require("../sidecars/vscode-bridge/protocol.cjs");
const { createBridge } = require("../sidecars/vscode-bridge/extension.cjs");
function event() {
  const listeners = new Set();
  return {
    on: (fn) => {
      listeners.add(fn);
      return { dispose: () => listeners.delete(fn) };
    },
    emit: (e) => {
      for (const listener of listeners) listener(e);
    },
  };
}
async function fixture(fn) {
  // Hosted Windows runners may expose TEMP through an 8.3 path alias.
  // Use the canonical owned path; production alias rejection stays enabled.
  const root = await realpath(
    await mkdtemp(path.join(tmpdir(), "ap-vscode-bridge-")),
  );
  await mkdir(path.join(root, ".vscode"));
  await writeFile(
    path.join(root, ".vscode/tasks.json"),
    '{"version":"2.0.0","tasks":[{"label":"build","command":"echo owned"}]}',
  );
  const ended = event(),
    processEnded = event(),
    started = event(),
    processStarted = event();
  let executed = 0,
    cancelled = 0,
    commands = 0;
  const uri = (file) => ({
    scheme: "file",
    fsPath: file,
    toString: () => `file://${file}`,
  });
  const task = {
    name: "build",
    source: "Workspace",
    scope: { uri: uri(root) },
    definition: { type: "shell" },
    execution: { command: "echo", args: ["owned"], options: {} },
    problemMatchers: [],
  };
  const vscode = {
    version: "fixture-1",
    workspace: {
      workspaceFolders: [{ uri: uri(root) }],
      isTrusted: true,
      textDocuments: [],
      openTextDocument: async (file) => ({ uri: file, version: 1 }),
    },
    extensions: { all: [] },
    languages: { getDiagnostics: () => [] },
    commands: {
      getCommands: async () => ["fixture.run", "_internal"],
      executeCommand: async () => {
        commands++;
        return { secret: "never exposed" };
      },
    },
    tasks: {
      fetchTasks: async () => [task],
      executeTask: async () => {
        executed++;
        return {
          terminate: () => {
            cancelled++;
          },
        };
      },
      onDidEndTask: ended.on,
      onDidEndTaskProcess: processEnded.on,
      onDidStartTask: started.on,
      onDidStartTaskProcess: processStarted.on,
    },
    Uri: { file: uri },
    window: { showTextDocument: async () => {} },
  };
  const session = randomUUID(),
    token = "d".repeat(64);
  const bridge = await createBridge(vscode, {
    version: 1,
    session,
    token,
    root,
    endpoint: path.join(root, "endpoint.json"),
  });
  async function call(operation, input = {}, extra = {}) {
    const id = extra.id || randomUUID();
    const response = await fetch(
      `http://127.0.0.1:${bridge.endpoint.port}/v1`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${extra.token || token}`,
          ...extra.headers,
        },
        body: JSON.stringify({
          id,
          session: extra.session || session,
          operation,
          input,
        }),
        redirect: "error",
      },
    );
    return { status: response.status, ...(await response.json()) };
  }
  try {
    await fn({
      root,
      vscode,
      task,
      ended,
      processEnded,
      started,
      processStarted,
      call,
      bridge,
      counts: () => ({ executed, cancelled, commands }),
    });
  } finally {
    await bridge.dispose();
    await rm(root, { recursive: true });
  }
}
test("JSONC handles BOM/comments/trailing commas without evaluating expressions or variables", () => {
  const value = P.jsonc(
    '\uFEFF/*owned*/{"tasks":[{"label":"á 测试", "command":"${command:fixture.run}",},],"url":"http://x//y"}//end',
  );
  assert.equal(value.tasks[0].command, "${command:fixture.run}");
  assert.equal(value.url, "http://x//y");
  for (const raw of [
    '{"a":1,"a":2}',
    '{"__proto__":1}',
    "{a:1}",
    '{"a":process.exit()}',
    '{"a":1e9999}',
    '{"a":01}',
    '{"a":"\\q"}',
  ])
    assert.throws(() => P.jsonc(raw), /VSCODE_CONFIG/);
});
test("parser depth, input size and protocol keys are bounded before API calls", () => {
  assert.throws(() => P.jsonc("[".repeat(40) + "0" + "]".repeat(40)), /LIMIT/);
  assert.throws(() => P.jsonc(" ".repeat(256 * 1024 + 1)), /LIMIT/);
  assert.throws(
    () =>
      P.request({
        id: randomUUID(),
        session: randomUUID(),
        operation: "diagnostics",
        input: { arbitrary: 1 },
      }),
    /VSCODE_INPUT/,
  );
  assert.throws(
    () =>
      P.request({
        id: randomUUID(),
        session: randomUUID(),
        operation: "command",
        input: {
          command: "_internal",
          arguments: [],
          fingerprint: "a".repeat(64),
        },
      }),
    /COMMAND_INPUT/,
  );
});
test("loopback auth, origin, session and replay guards do not execute commands", async () =>
  fixture(async ({ call, counts }) => {
    assert.equal((await call("info", {}, { token: "bad" })).status, 401);
    assert.equal(
      (await call("info", {}, { headers: { Origin: "https://example.test" } }))
        .status,
      403,
    );
    assert.equal(
      (await call("info", {}, { session: randomUUID() })).error,
      "VSCODE_SESSION",
    );
    const id = randomUUID();
    assert.equal((await call("info", {}, { id })).ok, true);
    assert.equal(
      (await call("info", {}, { id })).error,
      "VSCODE_REQUEST_REPLAY_DENIED",
    );
    assert.deepEqual(counts(), { executed: 0, cancelled: 0, commands: 0 });
  }));
test("public command observation is checked again, trust denied, arbitrary result omitted", async () =>
  fixture(async ({ call, vscode, counts }) => {
    const observation = await call("commands");
    assert.deepEqual(observation.data.commands, ["fixture.run"]);
    const input = {
      command: "fixture.run",
      arguments: ["literal Unicode ✓"],
      fingerprint: observation.data.fingerprint,
    };
    assert.equal(
      (await call("command", { ...input, fingerprint: "a".repeat(64) })).error,
      "VSCODE_COMMAND_OBSERVATION_CHANGED",
    );
    vscode.workspace.isTrusted = false;
    assert.equal(
      (await call("command", input)).error,
      "VSCODE_WORKSPACE_UNTRUSTED",
    );
    vscode.workspace.isTrusted = true;
    const result = await call("command", input);
    assert.equal(result.data.invoked, true);
    assert.equal(result.data.resultExposed, false);
    assert.ok(!JSON.stringify(result).includes("never exposed"));
    assert.equal(counts().commands, 1);
  }));
test("task review binds raw JSONC configuration, provider and execution, omits environment values", async () =>
  fixture(async ({ call, root, task, counts }) => {
    task.execution.options.env = { OWNED_VALUE: "private fixture value" };
    const observation = await call("tasks");
    assert.equal(observation.ok, true);
    assert.ok(!JSON.stringify(observation).includes("private fixture value"));
    assert.deepEqual(observation.data.tasks[0].execution.options.env.keys, [
      "OWNED_VALUE",
    ]);
    await writeFile(
      path.join(root, ".vscode/tasks.json"),
      '{"version":"2.0.0","tasks":[]}',
    );
    const input = {
      task: observation.data.tasks[0].id,
      fingerprint: observation.data.fingerprint,
    };
    assert.equal(
      (await call("run_task", input)).error,
      "VSCODE_TASK_OBSERVATION_CHANGED",
    );
    assert.equal(counts().executed, 0);
  }));
test("extension activation alone is stable; installed version changes invalidate observations", async () =>
  fixture(async ({ call, vscode }) => {
    const extension = {
      id: "fixture.provider",
      packageJSON: { version: "1" },
      isActive: false,
    };
    vscode.extensions.all = [extension];
    const first = await call("tasks");
    extension.isActive = true;
    assert.equal(
      (await call("tasks")).data.fingerprint,
      first.data.fingerprint,
    );
    extension.packageJSON.version = "2";
    assert.notEqual(
      (await call("tasks")).data.fingerprint,
      first.data.fingerprint,
    );
  }));
test("literal secret fields and quoted JSON environment credentials are denied", async () =>
  fixture(async ({ call, task, counts }) => {
    task.execution.options.env = { PRIVATE_TOKEN: "owned_fixture_value" };
    assert.equal(
      (await call("tasks")).error,
      "VSCODE_TASK_LITERAL_SECRET_DENIED",
    );
    assert.equal(counts().executed, 0);
  }));
test("early owned cancellation repeats at process start and retains early exit events", async () =>
  fixture(
    async ({
      call,
      vscode,
      started,
      processStarted,
      ended,
      processEnded,
      counts,
    }) => {
      let execution = { terminate() {} };
      const realExecute = vscode.tasks.executeTask;
      vscode.tasks.executeTask = async (task) => {
        execution = await realExecute(task);
        return execution;
      };
      let observed = await call("tasks"),
        run = await call("run_task", {
          task: observed.data.tasks[0].id,
          fingerprint: observed.data.fingerprint,
        });
      await call("task_cancel", { run: run.data.id });
      assert.equal(counts().cancelled, 1);
      started.emit({ execution });
      processStarted.emit({ execution, processId: 1234 });
      assert.equal(counts().cancelled, 3);
      processEnded.emit({ execution, exitCode: 7 });
      ended.emit({ execution });
      const state = (await call("task_status", { run: run.data.id })).data;
      assert.equal(state.state, "ended");
      assert.equal(state.exitCode, 7);
      vscode.tasks.executeTask = async (task) => {
        const ex = await realExecute(task);
        processEnded.emit({ execution: ex, exitCode: 9 });
        ended.emit({ execution: ex });
        return ex;
      };
      observed = await call("tasks");
      run = await call("run_task", {
        task: observed.data.tasks[0].id,
        fingerprint: observed.data.fingerprint,
      });
      assert.equal(run.data.state, "ended");
      assert.equal(run.data.exitCode, 9);
    },
  ));
test("only returned TaskExecution is cancellable; process exit and task end are separate", async () =>
  fixture(async ({ call, vscode, ended, processEnded, counts }) => {
    const observation = await call("tasks");
    const started = await call("run_task", {
      task: observation.data.tasks[0].id,
      fingerprint: observation.data.fingerprint,
    });
    assert.equal(started.ok, true);
    assert.equal(started.data.state, "running");
    assert.equal(started.data.terminalOutputExposed, false);
    assert.equal(
      (await call("task_cancel", { run: randomUUID() })).error,
      "VSCODE_RUN_NOT_OWNED",
    );
    const execution = { terminate() {} }; // foreign task events cannot mark ours ended
    processEnded.emit({ execution, exitCode: 0 });
    ended.emit({ execution });
    assert.equal(
      (await call("task_status", { run: started.data.id })).data.state,
      "running",
    );
    assert.equal(
      (await call("task_cancel", { run: started.data.id })).data
        .cancellationRequested,
      true,
    );
    assert.equal(counts().cancelled, 1);
    assert.equal((await call("revoke")).data.editorPreserved, true);
    assert.equal((await call("info")).error, "VSCODE_SESSION_EXPIRED");
  }));
test("diagnostics expose available scoped results, empty is not compilation success", async () =>
  fixture(async ({ call, root, vscode }) => {
    const file = path.join(root, "owned.txt");
    await writeFile(file, "owned");
    const diagnostic = {
      severity: 0,
      message: "Owned diagnostic",
      source: "fixture",
      range: {
        start: { line: 2, character: 4 },
        end: { line: 2, character: 6 },
      },
    };
    vscode.languages.getDiagnostics = () => [
      [{ scheme: "file", fsPath: file }, [diagnostic]],
      [{ scheme: "untitled" }, [diagnostic]],
    ];
    const result = await call("diagnostics");
    assert.equal(result.ok, true);
    assert.equal(result.data.items.length, 1);
    assert.equal(result.data.items[0].range.start.line, 3);
    assert.equal(result.data.skippedOutsideScopeOrUnavailable, 1);
    assert.equal(result.data.complete, false);
    assert.equal(result.data.compilationVerified, false);
    vscode.languages.getDiagnostics = () => [];
    assert.equal((await call("diagnostics")).data.compilationVerified, false);
  }));
test("workspace replacement denies all operations and no foreign editor file opens", async () =>
  fixture(async ({ call, vscode, root }) => {
    const file = path.join(root, "owned file & ü.txt");
    await writeFile(file, "fixture");
    vscode.workspace.isTrusted = false;
    assert.equal(
      (await call("open", { path: file })).error,
      "VSCODE_WORKSPACE_UNTRUSTED",
    );
    vscode.workspace.isTrusted = true;
    assert.equal(
      (await call("open", { path: file, line: 0 })).error,
      "VSCODE_OPEN_INPUT",
    );
    assert.equal((await call("open", { path: file })).data.opened, true);
    assert.equal(await readFile(file, "utf8"), "fixture");
    vscode.workspace.workspaceFolders = [];
    assert.equal((await call("commands")).error, "VSCODE_WORKSPACE_CHANGED");
  }));
