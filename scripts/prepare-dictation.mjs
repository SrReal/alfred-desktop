import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile, readdir } from "node:fs/promises";
import { resolve } from "node:path";

export async function prepareDictation(root) {
  if (process.platform !== "win32") throw new Error("WINDOWS_11_REQUIRED");
  const windows = process.env.SystemRoot;
  if (!windows) throw new Error("WINDOWS_SYSTEM_ROOT");
  const framework = resolve(windows, "Microsoft.NET/Framework64/v4.0.30319");
  const speechRoot = resolve(
    windows,
    "Microsoft.NET/assembly/GAC_MSIL/System.Speech",
  );
  const speechVersion = (await readdir(speechRoot)).find((name) =>
    /^v4\.0_4\.0\.0\.0__31bf3856ad364e35$/i.test(name),
  );
  if (!speechVersion) throw new Error("DICTATION_BUILD_SYSTEM_SPEECH");
  const output = resolve(root, ".work/dictation-host");
  await mkdir(output, { recursive: true });
  const source = resolve(root, "native/dictation.cs");
  const binary = resolve(output, "AlfredPet.Dictation.exe");
  const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
  const sourceSha256 = hash(await readFile(source));
  const manifestPath = resolve(output, "manifest.json");
  let previous;
  try {
    previous = JSON.parse(await readFile(manifestPath, "utf8"));
  } catch {
    /* First build. */
  }
  if (previous?.sourceSha256 === sourceSha256) {
    try {
      if (hash(await readFile(binary)) === previous.executableSha256)
        return previous;
    } catch {
      /* Rebuild missing output. */
    }
  }
  await promisify(execFile)(
    resolve(framework, "csc.exe"),
    [
      "/nologo",
      "/platform:x64",
      "/optimize+",
      "/debug-",
      "/target:exe",
      "/out:AlfredPet.Dictation.exe",
      `/reference:${resolve(speechRoot, speechVersion, "System.Speech.dll")}`,
      `/reference:${resolve(framework, "System.Web.Extensions.dll")}`,
      source.replaceAll("/", "\\"),
    ],
    { cwd: output, windowsHide: true, timeout: 60000 },
  );
  const manifest = {
    sourceSha256,
    executableSha256: hash(await readFile(binary)),
    runtime: "Windows .NET Framework 4 / System.Speech",
    microphoneActivatedDuringBuild: false,
  };
  await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
  return manifest;
}
