import { readFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import { extname, resolve } from "node:path";
import assert from "node:assert/strict";

// Release packaging must include the reviewed guide matching this application.
const root = resolve(import.meta.dirname, "../..");
const read = (path) => readFile(resolve(root, path));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const textSuffixes = new Set([
  ".rs",
  ".toml",
  ".lock",
  ".json",
  ".ts",
  ".tsx",
  ".js",
  ".mjs",
  ".cjs",
  ".cs",
  ".ps1",
  ".md",
  ".yml",
  ".yaml",
  ".html",
  ".css",
  ".svg",
  ".cmd",
  ".bat",
  ".txt",
]);
const canonicalText = (bytes) =>
  Buffer.from(bytes.toString("utf8").replace(/\r\n/g, "\n"));
const proof = JSON.parse(await read("docs/manual/verification.json"));
const manual = JSON.parse(await read("docs/manual/manual.es.json"));
assert.equal(proof.status, "PASS", "Review the manual before packaging");
assert.equal(proof.visualReview?.pagesReviewed, manual.pages.length + 2);
assert.equal(
  hash(canonicalText(await read("docs/manual/manual.es.json"))),
  proof.contentSha256,
);
assert.equal(
  hash(canonicalText(await read("docs/manual/build_manual.py"))),
  proof.builderSha256,
);
const pdf = await read("output/pdf/AlfredPet-Manual-de-uso.pdf");
assert.equal(pdf.subarray(0, 5).toString(), "%PDF-");
assert.equal(hash(pdf), proof.pdfSha256, "PDF differs from the reviewed guide");
for (const [path, expected] of Object.entries(proof.images)) {
  assert.equal(
    hash(await read(path)),
    expected,
    `Manual image changed: ${path}`,
  );
}
const { stdout } = await promisify(execFile)(
  "git",
  ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
  { cwd: root, maxBuffer: 8 * 1024 * 1024 },
);
const files = [
  ...new Set(
    stdout
      .split("\0")
      .filter(
        (path) =>
          /^(src\/|src-tauri\/|native\/|sidecars\/|assets\/)/.test(path) ||
          [
            "package.json",
            "package-lock.json",
            "index.html",
            "vite.config.ts",
          ].includes(path),
      ),
  ),
].sort();
const application = createHash("sha256");
for (const path of files) {
  application.update(path + "\0");
  const bytes = await read(path);
  application.update(
    textSuffixes.has(extname(path).toLowerCase())
      ? canonicalText(bytes)
      : bytes,
  );
  application.update("\0");
}
assert.equal(
  application.digest("hex"),
  proof.applicationSha256,
  "Application changed: update and review the manual before packaging",
);
const config = JSON.parse(await read("src-tauri/tauri.conf.json"));
assert.equal(
  config.bundle.resources["../output/pdf/AlfredPet-Manual-de-uso.pdf"],
  "manual/AlfredPet-Manual-de-uso.pdf",
);
console.log(
  "Reviewed offline manual matches the application and release resource mapping.",
);
