"use strict";
// Test extension loaded only in an owned UUID editor profile, never bundled.
const vscode = require("vscode");
const fs = require("node:fs/promises");
const path = require("node:path");
exports.activate = (context) => {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!root || !path.basename(root).startsWith("ap-vscode-owned-"))
    throw new Error("OWNED_FIXTURE_ONLY");
  const diagnostics = vscode.languages.createDiagnosticCollection(
    "alfredpet-owned-fixture",
  );
  const diagnostic = new vscode.Diagnostic(
    new vscode.Range(0, 1, 0, 3),
    "Owned fixture diagnostic á 测试",
    vscode.DiagnosticSeverity.Warning,
  );
  diagnostic.source = "AlfredPet owned fixture";
  diagnostics.set(vscode.Uri.file(path.join(root, "owned.ts")), [diagnostic]);
  context.subscriptions.push(
    diagnostics,
    vscode.commands.registerCommand("alfredpet.fixture.touch", async () => {
      await fs.writeFile(
        path.join(root, "command-observed.txt"),
        "owned command á 测试",
        { flag: "wx" },
      );
      return { content: "omitted result" };
    }),
  );
};
