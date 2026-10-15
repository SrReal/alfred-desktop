import { mkdir, writeFile, readFile, cp } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { prepareSemantic } from "./prepare-semantic.mjs";
import { prepareDictation } from "./prepare-dictation.mjs";
const root = resolve(import.meta.dirname, "..");
const nodeVersion = "v24.21.0";
const nodeSha256 =
  "ba4e6d110e8c1592a1ecd390f6b05f3da124b13871a5be62b341a07a853c6c32";
if (process.version !== nodeVersion) {
  throw new Error(
    `AlfredPet runtime: expected Node ${nodeVersion}; update the runtime manifest and license before bundling another version.`,
  );
}
await mkdir(resolve(root, "src-tauri/icons"), { recursive: true });
await mkdir(resolve(root, "assets/audio"), { recursive: true });
// Brand assets are reviewed and checked in; never replace them with placeholders.
const branding = JSON.parse(
  await readFile(resolve(root, "assets/branding/manifest.json"), "utf8"),
);
for (const [path, expected] of Object.entries(branding.files)) {
  const bytes = await readFile(resolve(root, path));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `AlfredPet icon differs from its branding manifest: ${path}`,
    );
  }
}
const sampleRate = 16000;
const n = 1600;
const wav = Buffer.alloc(44 + n * 2);
wav.write("RIFF");
wav.writeUInt32LE(wav.length - 8, 4);
wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16);
wav.writeUInt16LE(1, 20);
wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(sampleRate, 24);
wav.writeUInt32LE(sampleRate * 2, 28);
wav.writeUInt16LE(2, 32);
wav.writeUInt16LE(16, 34);
wav.write("data", 36);
wav.writeUInt32LE(n * 2, 40);
for (let i = 0; i < n; i++)
  wav.writeInt16LE(
    Math.round(Math.sin((2 * Math.PI * 440 * i) / sampleRate) * 1000),
    44 + i * 2,
  );
await writeFile(resolve(root, "assets/audio/attention-test.wav"), wav);
await mkdir(resolve(root, ".work/runtime"), { recursive: true });
await cp(
  resolve(root, "assets/licenses/Node-v24.21.0-LICENSE.txt"),
  resolve(root, ".work/runtime/LICENSE-node.txt"),
);
await cp(process.execPath, resolve(root, ".work/runtime/node.exe"));
await cp(
  resolve(root, "node_modules/playwright-core"),
  resolve(root, ".work/browser-host/node_modules/playwright-core"),
  { recursive: true },
);
await cp(
  resolve(root, "sidecars/browser-host/host.mjs"),
  resolve(root, ".work/browser-host/host.mjs"),
);
await cp(
  resolve(root, "sidecars/browser-host/forms.mjs"),
  resolve(root, ".work/browser-host/forms.mjs"),
);
const binary = await readFile(process.execPath);
if (createHash("sha256").update(binary).digest("hex") !== nodeSha256) {
  throw new Error(
    "AlfredPet runtime hash differs from the verified Node binary; inspect it before packaging.",
  );
}
await mkdir(resolve(root, "docs/evidence"), { recursive: true });
await writeFile(
  resolve(root, "docs/evidence/browser-runtime.json"),
  JSON.stringify(
    {
      nodeVersion: process.version,
      sha256: createHash("sha256").update(binary).digest("hex"),
      playwright: JSON.parse(
        await readFile(
          resolve(root, "node_modules/playwright-core/package.json"),
          "utf8",
        ),
      ).version,
    },
    null,
    2,
  ) + "\n",
);
await prepareSemantic(root);
await prepareDictation(root);
console.log(
  "AlfredPet face icons verified; WAV fixture and bundled browser runtime prepared.",
);
