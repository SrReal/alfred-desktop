"use strict";
// This companion never calls AlfredPet's MCP server. It serves only its own
// explicit editor session, with an independent ephemeral bearer and lease.
const fs = require("node:fs/promises");
const path = require("node:path");
const http = require("node:http");
const crypto = require("node:crypto");
const { TextDecoder } = require("node:util");
const P = require("./protocol.cjs");
let active;
async function createBridge(vscode, bootstrap) {
  P.keys(bootstrap, ["version", "session", "token", "root", "endpoint"]);
  if (
    bootstrap.version !== 1 ||
    !P.uuid(bootstrap.session) ||
    !/^[a-f0-9]{64}$/.test(bootstrap.token) ||
    !P.text(bootstrap.root) ||
    !P.text(bootstrap.endpoint)
  )
    P.fail("VSCODE_BOOTSTRAP");
  await P.localPath(bootstrap.root, bootstrap.root, true);
  const now = () => Number(process.hrtime.bigint() / 1000000n);
  let deadline = now() + 15000,
    revoked = false,
    busy = false;
  const seen = new Set(),
    runs = new Map(),
    subscriptions = [],
    endings = new WeakMap(),
    bearer = Buffer.from(`Bearer ${bootstrap.token}`);
  function scope() {
    const folders = vscode.workspace.workspaceFolders || [];
    if (revoked || now() >= deadline) P.fail("VSCODE_SESSION_EXPIRED");
    if (
      folders.length !== 1 ||
      folders[0].uri.scheme !== "file" ||
      !P.same(folders[0].uri.fsPath, bootstrap.root) ||
      vscode.workspace.workspaceFile
    )
      P.fail("VSCODE_WORKSPACE_CHANGED");
  }
  function terminateOwned() {
    revoked = true;
    for (const run of runs.values())
      if (run.state === "running") {
        run.cancellationRequested = true;
        try {
          run.execution.terminate();
        } catch {
          run.cancellationError = "VSCODE_TASK_TERMINATE_FAILED";
        }
      }
  }
  function info() {
    scope();
    return {
      session: bootstrap.session,
      root: bootstrap.root,
      trusted: vscode.workspace.isTrusted === true,
      editorVersion: vscode.version,
      languageDiagnosticsExposed: true,
      diagnosticsComplete: false,
      taskTerminalOutputExposed: false,
      managedProfile: true,
      commandCancellationExposed: false,
      providerState: "unknown",
      observedAt: new Date().toISOString(),
    };
  }
  // Activation changes during normal discovery. Bind installed code identity,
  // actual task/command contents and trust, not transient isActive status.
  const extensions = () =>
    vscode.extensions.all
      .map((e) => ({
        id: e.id,
        version: e.packageJSON.version || "unknown",
        location: e.extensionUri?.toString() || "unknown",
      }))
      .sort((a, b) => a.id.localeCompare(b.id));
  async function configurations() {
    const location = path.join(bootstrap.root, ".vscode", "tasks.json");
    try {
      await P.localPath(location, bootstrap.root);
      const handle = await fs.open(location, "r");
      let bytes;
      try {
        if ((await handle.stat()).size > 256 * 1024)
          P.fail("VSCODE_CONFIG_LIMIT");
        bytes = await handle.readFile();
      } finally {
        await handle.close();
      }
      if (bytes.length > 256 * 1024) P.fail("VSCODE_CONFIG_LIMIT");
      const parsed = P.jsonc(
        new TextDecoder("utf-8", { fatal: true }).decode(bytes),
      );
      if (
        !P.object(parsed) ||
        (parsed.tasks !== undefined && !Array.isArray(parsed.tasks)) ||
        (parsed.tasks || []).length > 100
      )
        P.fail("VSCODE_CONFIG_TASKS");
      return { hash: P.hash(bytes.toString("hex")), data: parsed };
    } catch (error) {
      if (error.code === "ENOENT") return { hash: P.hash("absent"), data: {} };
      throw error;
    }
  }
  async function commandObservation() {
    scope();
    const commands = [...new Set(await vscode.commands.getCommands(true))]
      .filter((c) => P.text(c, 200) && !c.startsWith("_"))
      .sort();
    if (commands.length > 10000) P.fail("VSCODE_COMMAND_LIMIT");
    scope();
    const context = {
      session: bootstrap.session,
      root: bootstrap.root,
      trusted: vscode.workspace.isTrusted === true,
      extensions: extensions(),
      commands,
    };
    return {
      commands,
      fingerprint: P.hash(context),
      observedAt: new Date().toISOString(),
      resultsUntrusted: true,
    };
  }
  function executionDescription(task) {
    const execution = task.execution;
    let data;
    if (!execution) data = { type: "none" };
    else if (typeof execution.process === "string")
      data = {
        type: "process",
        process: execution.process,
        args: execution.args || [],
        options: execution.options || {},
      };
    else if (
      typeof execution.commandLine === "string" ||
      typeof execution.command === "string" ||
      P.object(execution.command)
    )
      data = {
        type: "shell",
        command: execution.commandLine || execution.command,
        args: execution.args || [],
        options: execution.options || {},
      };
    else
      data = {
        type: "custom",
        review: "Provider code is opaque; may execute arbitrary code.",
      };
    return P.canonical(data);
  }
  function safeReview(v) {
    const string = JSON.stringify(v);
    if (P.secret(string)) P.fail("VSCODE_TASK_LITERAL_SECRET_DENIED");
    const result = P.canonical(v);
    function omitEnv(node) {
      if (!node || typeof node !== "object") return;
      for (const k of Object.keys(node)) {
        if (k.toLowerCase() === "env" && P.object(node[k]))
          node[k] = { valuesOmitted: true, keys: Object.keys(node[k]).sort() };
        else omitEnv(node[k]);
      }
    }
    omitEnv(result);
    return result;
  }
  async function taskObservation() {
    scope();
    const before = await configurations();
    const tasks = await vscode.tasks.fetchTasks();
    if (tasks.length > 100) P.fail("VSCODE_TASK_LIMIT");
    scope();
    const after = await configurations();
    if (before.hash !== after.hash) P.fail("VSCODE_TASK_CONFIGURATION_CHANGED");
    const descriptions = tasks.map((task) => {
      const taskScope =
        typeof task.scope === "number"
          ? task.scope
          : task.scope?.uri?.scheme === "file"
            ? task.scope.uri.fsPath
            : "unknown";
      if (typeof taskScope === "string" && !P.same(taskScope, bootstrap.root))
        P.fail("VSCODE_TASK_SCOPE");
      return P.canonical({
        name: task.name,
        source: task.source,
        scope: taskScope,
        definition: task.definition,
        execution: executionDescription(task),
        group: task.group
          ? { id: task.group.id, isDefault: !!task.group.isDefault }
          : null,
        problemMatchers: task.problemMatchers || [],
        isBackground: !!task.isBackground,
        presentationOptions: task.presentationOptions || {},
        runOptions: task.runOptions || {},
      });
    });
    const identities = descriptions.map(P.hash);
    if (new Set(identities).size !== identities.length)
      P.fail("VSCODE_TASK_AMBIGUOUS");
    const context = {
      session: bootstrap.session,
      root: bootstrap.root,
      trusted: vscode.workspace.isTrusted === true,
      extensions: extensions(),
      configHash: after.hash,
      tasks: descriptions
        .slice()
        .sort((a, b) => P.hash(a).localeCompare(P.hash(b))),
    };
    const fingerprint = P.hash(context);
    const fingerprintDetails = {
      extensions: P.hash(context.extensions),
      configuration: after.hash,
      tasks: P.hash(context.tasks),
      trusted: context.trusted,
    };
    return {
      fingerprint,
      fingerprintDetails,
      tasks: descriptions.map((description, n) => ({
        id: identities[n],
        ...safeReview(description),
      })),
      configuredTasks: safeReview(after.data),
      resolution:
        "VS Code resolves variables, inputs, dependencies and provider code at execution; review does not sandbox their effects.",
      taskTerminalOutputExposed: false,
      observedAt: new Date().toISOString(),
      resultsUntrusted: true,
      _objects: tasks,
      _ids: identities,
    };
  }
  subscriptions.push(
    vscode.tasks.onDidEndTaskProcess((event) => {
      endings.set(event.execution, {
        ...endings.get(event.execution),
        exitCode: Number.isInteger(event.exitCode) ? event.exitCode : null,
        processEndedAt: new Date().toISOString(),
      });
      for (const run of runs.values())
        if (run.execution === event.execution) {
          run.exitCode = Number.isInteger(event.exitCode)
            ? event.exitCode
            : null;
          run.processEndedAt = new Date().toISOString();
        }
    }),
  );
  subscriptions.push(
    vscode.tasks.onDidEndTask((event) => {
      endings.set(event.execution, {
        ...endings.get(event.execution),
        state: "ended",
        endedAt: new Date().toISOString(),
      });
      for (const run of runs.values())
        if (run.execution === event.execution) {
          run.state = "ended";
          run.endedAt = new Date().toISOString();
        }
    }),
  );
  function started(event, processStarted) {
    const data = processStarted
      ? { processStarted: true, processId: event.processId }
      : { taskStarted: true };
    endings.set(event.execution, { ...endings.get(event.execution), ...data });
    for (const run of runs.values())
      if (run.execution === event.execution) {
        Object.assign(run, data);
        // VS Code may return TaskExecution before its process is created. An
        // early terminate can be ineffective; repeat for this exact owned
        // execution when its start event arrives, never for a foreign task.
        if (run.cancellationRequested && run.state === "running") {
          try {
            run.execution.terminate();
          } catch {
            run.cancellationError = "VSCODE_TASK_TERMINATE_FAILED";
          }
        }
      }
  }
  subscriptions.push(
    vscode.tasks.onDidStartTask((event) => started(event, false)),
  );
  subscriptions.push(
    vscode.tasks.onDidStartTaskProcess((event) => started(event, true)),
  );
  function runState(run) {
    const { execution, ...value } = run;
    return value;
  }
  async function perform(operation, input) {
    scope();
    if (operation === "lease") {
      deadline = now() + 15000;
      return { renewed: true };
    }
    if (operation === "revoke") {
      terminateOwned();
      return { revoked: true, editorPreserved: true };
    }
    if (operation === "info") return info();
    if (operation === "commands") return commandObservation();
    if (operation === "tasks") {
      const { _objects, _ids, ...result } = await taskObservation();
      return result;
    }
    if (operation === "diagnostics") {
      const entries = vscode.languages.getDiagnostics();
      if (entries.length > 1000) P.fail("VSCODE_DIAGNOSTICS_LIMIT");
      const items = [];
      let skipped = 0;
      for (const [uri, diagnostics] of entries) {
        if (uri.scheme !== "file") {
          skipped += diagnostics.length;
          continue;
        }
        let file;
        try {
          file = await P.localPath(uri.fsPath, bootstrap.root);
        } catch {
          skipped += diagnostics.length;
          continue;
        }
        for (const d of diagnostics) {
          if (items.length >= 1000) P.fail("VSCODE_DIAGNOSTICS_LIMIT");
          items.push({
            path: file,
            severity: d.severity,
            message: String(d.message).slice(0, 8000),
            source: d.source || null,
            code: P.object(d.code)
              ? String(d.code.value)
              : d.code === undefined
                ? null
                : String(d.code),
            range: {
              start: {
                line: d.range.start.line + 1,
                column: d.range.start.character + 1,
              },
              end: {
                line: d.range.end.line + 1,
                column: d.range.end.character + 1,
              },
            },
            documentVersion:
              vscode.workspace.textDocuments.find(
                (doc) => doc.uri.toString() === uri.toString(),
              )?.version ?? null,
          });
        }
      }
      scope();
      return {
        items: safeReview(items),
        skippedOutsideScopeOrUnavailable: skipped,
        exposed: true,
        complete: false,
        providerState: "unknown",
        compilationVerified: false,
        observedAt: new Date().toISOString(),
        resultsUntrusted: true,
      };
    }
    if (operation === "task_status" || operation === "task_cancel") {
      const run = runs.get(input.run);
      if (!run) P.fail("VSCODE_RUN_NOT_OWNED");
      if (operation === "task_cancel" && run.state === "running") {
        scope();
        run.cancellationRequested = true;
        run.execution.terminate();
      }
      return runState(run);
    }
    if (vscode.workspace.isTrusted !== true)
      P.fail("VSCODE_WORKSPACE_UNTRUSTED");
    if (operation === "command") {
      const observation = await commandObservation();
      if (
        observation.fingerprint !== input.fingerprint ||
        !observation.commands.includes(input.command)
      )
        P.fail("VSCODE_COMMAND_OBSERVATION_CHANGED");
      scope();
      await vscode.commands.executeCommand(input.command, ...input.arguments);
      // Arbitrary API result objects may contain credentials or cycles. Omit.
      return {
        invoked: true,
        command: input.command,
        resultExposed: false,
        effectsVerified: false,
      };
    }
    if (operation === "run_task") {
      const observation = await taskObservation();
      const index = observation._ids.indexOf(input.task);
      if (observation.fingerprint !== input.fingerprint || index < 0)
        P.fail("VSCODE_TASK_OBSERVATION_CHANGED");
      if (runs.size >= 128) {
        for (const [id, run] of runs) {
          if (run.state !== "running") runs.delete(id);
          if (runs.size < 64) break;
        }
      }
      if (runs.size >= 128) P.fail("VSCODE_RUN_LIMIT");
      scope();
      const id = crypto.randomUUID(),
        execution = await vscode.tasks.executeTask(observation._objects[index]);
      const run = {
        id,
        task: input.task,
        state: "running",
        execution,
        exitCode: null,
        cancellationRequested: false,
        startedAt: new Date().toISOString(),
        terminalOutputExposed: false,
      };
      Object.assign(run, endings.get(execution));
      runs.set(id, run);
      if (revoked || now() >= deadline) {
        run.cancellationRequested = true;
        execution.terminate();
      }
      return runState(run);
    }
    if (operation === "open") {
      const file = await P.localPath(input.path, bootstrap.root);
      scope();
      const document = await vscode.workspace.openTextDocument(
        vscode.Uri.file(file),
      );
      const options = { preview: true, preserveFocus: false };
      if (input.line)
        options.selection = new vscode.Range(
          new vscode.Position(input.line - 1, (input.column || 1) - 1),
          new vscode.Position(input.line - 1, (input.column || 1) - 1),
        );
      await vscode.window.showTextDocument(document, options);
      return { opened: true, path: file, documentVersion: document.version };
    }
    P.fail("VSCODE_OPERATION");
  }
  const server = http.createServer(
    { maxHeaderSize: 8192, requestTimeout: 10000, headersTimeout: 10000 },
    async (req, res) => {
      const send = (status, value) => {
        if (res.destroyed) return;
        let body = JSON.stringify(value);
        if (Buffer.byteLength(body) > P.LIMIT) {
          status = 413;
          body = JSON.stringify({ ok: false, error: "VSCODE_RESPONSE_LIMIT" });
        }
        res.writeHead(status, {
          "Content-Type": "application/json; charset=utf-8",
          "Cache-Control": "no-store",
          "X-Content-Type-Options": "nosniff",
        });
        res.end(body);
      };
      if (
        req.method !== "POST" ||
        req.url !== "/v1" ||
        req.headers.origin ||
        req.headers["content-type"] !== "application/json" ||
        req.socket.remoteAddress !== "127.0.0.1"
      ) {
        req.resume();
        send(403, { ok: false, error: "VSCODE_HTTP_DENIED" });
        return;
      }
      const auth = Buffer.from(req.headers.authorization || "");
      if (
        auth.length !== bearer.length ||
        !crypto.timingSafeEqual(auth, bearer)
      ) {
        req.resume();
        send(401, { ok: false, error: "VSCODE_AUTH_DENIED" });
        return;
      }
      let request,
        mutation = false;
      try {
        const parts = [];
        let size = 0;
        for await (const bytes of req) {
          size += bytes.length;
          if (size > 64 * 1024) P.fail("VSCODE_INPUT_LIMIT");
          parts.push(bytes);
        }
        request = P.request(
          P.jsonc(
            new TextDecoder("utf-8", { fatal: true }).decode(
              Buffer.concat(parts),
            ),
          ),
        );
        if (request.session !== bootstrap.session) P.fail("VSCODE_SESSION");
        scope();
        if (seen.has(request.id)) P.fail("VSCODE_REQUEST_REPLAY_DENIED");
        if (request.operation !== "lease") {
          if (seen.size >= 4096) P.fail("VSCODE_SESSION_REQUEST_LIMIT");
          seen.add(request.id);
        }
        mutation = ["command", "run_task", "task_cancel", "open"].includes(
          request.operation,
        );
        if (mutation && busy) P.fail("VSCODE_ACTION_BUSY");
        if (mutation) busy = true;
        const operation = perform(request.operation, request.input);
        operation
          .finally(() => {
            if (mutation) busy = false;
          })
          .catch(() => {});
        let timer;
        try {
          const data = await Promise.race([
            operation,
            new Promise((_, reject) => {
              timer = setTimeout(
                () =>
                  reject(
                    new Error(
                      mutation
                        ? "VSCODE_ACTION_OUTCOME_UNCERTAIN"
                        : "VSCODE_API_TIMEOUT",
                    ),
                  ),
                30000,
              );
              timer.unref();
            }),
          ]);
          send(200, {
            id: request.id,
            session: bootstrap.session,
            ok: true,
            data,
          });
        } finally {
          clearTimeout(timer);
        }
      } catch (error) {
        const code = /^VSCODE_[A-Z_]+$/.test(error.message || "")
          ? error.message
          : mutation
            ? "VSCODE_ACTION_OUTCOME_UNCERTAIN"
            : "VSCODE_API_FAILED";
        send(400, {
          id: request?.id || null,
          session: bootstrap.session,
          ok: false,
          error: code,
        });
      }
    },
  );
  server.maxConnections = 8;
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const expiry = setInterval(() => {
    try {
      scope();
    } catch {
      terminateOwned();
      server.close();
    }
  }, 1000);
  expiry.unref();
  return {
    endpoint: {
      version: 1,
      session: bootstrap.session,
      port: server.address().port,
      extensionPid: process.pid,
    },
    info,
    dispose: async () => {
      terminateOwned();
      clearInterval(expiry);
      for (const sub of subscriptions) sub.dispose();
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      bearer.fill(0);
    },
  };
}
async function activate(context) {
  const vscode = require("vscode"),
    location = process.env.ALFREDPET_VSCODE_BOOTSTRAP;
  if (!location) return;
  delete process.env.ALFREDPET_VSCODE_BOOTSTRAP;
  // Core creates this directory with a protected current-user DACL. We never
  // write the bearer to endpoint attestation, editor output, or user settings.
  const parent = path.dirname(location);
  await P.localPath(location, parent);
  const bytes = await fs.readFile(location);
  if (bytes.length > 16384) P.fail("VSCODE_BOOTSTRAP_LIMIT");
  const bootstrap = P.jsonc(
    new TextDecoder("utf-8", { fatal: true }).decode(bytes),
  );
  if (!P.same(bootstrap.endpoint, path.join(parent, "endpoint.json")))
    P.fail("VSCODE_BOOTSTRAP_ENDPOINT");
  active = await createBridge(vscode, bootstrap);
  try {
    await P.localPath(bootstrap.endpoint, parent);
    const handle = await fs.open(bootstrap.endpoint, "r+");
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.nlink !== 1 || stat.size !== 0)
        P.fail("VSCODE_ENDPOINT_NOT_EMPTY");
      await handle.writeFile(JSON.stringify(active.endpoint));
      await handle.sync();
    } finally {
      await handle.close();
    }
  } catch {
    await active.dispose();
    active = undefined;
    P.fail("VSCODE_ENDPOINT_WRITE");
  }
  context.subscriptions.push({
    dispose() {
      active?.dispose().catch(() => {});
      active = undefined;
    },
  });
}
async function deactivate() {
  if (active) await active.dispose();
  active = undefined;
}
module.exports = { activate, deactivate, createBridge };
