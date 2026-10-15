import { cp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve, sep } from "node:path";

export async function prepareSemantic(root) {
  const target = resolve(root, ".work/semantic-host");
  const marker = resolve(target, ".alfredpet-generated");
  if (!target.startsWith(resolve(root, ".work") + sep))
    throw new Error("SEMANTIC_STAGE_PATH");
  if (await stat(target).catch(() => null)) {
    if (
      (await readFile(marker, "utf8").catch(() => "")) !== "semantic-host-v1\n"
    )
      throw new Error("SEMANTIC_STAGE_UNOWNED");
    await rm(target, { recursive: true });
  }
  await mkdir(target, { recursive: true });
  await writeFile(marker, "semantic-host-v1\n");
  const versions = {
    "@huggingface/tokenizers": "0.2.0",
    "onnxruntime-node": "1.30.0",
    "onnxruntime-common": "1.30.0",
  };
  for (const [name, version] of Object.entries(versions)) {
    const source = resolve(root, "node_modules", name);
    const destination = resolve(target, "node_modules", name);
    const pkg = JSON.parse(
      await readFile(resolve(source, "package.json"), "utf8"),
    );
    if (pkg.version !== version) throw new Error("SEMANTIC_RUNTIME_VERSION");
    await mkdir(destination, { recursive: true });
    for (const entry of ["package.json", "dist", "LICENSE", "README.md"])
      if (await stat(resolve(source, entry)).catch(() => null))
        await cp(resolve(source, entry), resolve(destination, entry), {
          recursive: true,
        });
    if (name === "onnxruntime-node") {
      const native = resolve(destination, "bin/napi-v6/win32/x64");
      await mkdir(native, { recursive: true });
      for (const file of [
        "onnxruntime_binding.node",
        "onnxruntime.dll",
        "DirectML.dll",
      ])
        await cp(
          resolve(source, "bin/napi-v6/win32/x64", file),
          resolve(native, file),
        );
    }
  }
  await cp(
    resolve(root, "sidecars/semantic-host/host.mjs"),
    resolve(target, "host.mjs"),
  );
  const nativeManifest = JSON.parse(
    await readFile(
      resolve(root, "assets/models/semantic-runtime.json"),
      "utf8",
    ),
  );
  if (
    nativeManifest.version !== versions["onnxruntime-node"] ||
    nativeManifest.platform !== "win32-x64"
  )
    throw new Error("SEMANTIC_RUNTIME_MANIFEST");
  for (const file of nativeManifest.files) {
    if (!/^[A-Za-z0-9_.]+$/.test(file.path))
      throw new Error("SEMANTIC_RUNTIME_MANIFEST");
    const bytes = await readFile(
      resolve(
        target,
        "node_modules/onnxruntime-node/bin/napi-v6/win32/x64",
        file.path,
      ),
    );
    if (
      bytes.length !== file.bytes ||
      createHash("sha256").update(bytes).digest("hex") !== file.sha256
    )
      throw new Error("SEMANTIC_RUNTIME_HASH");
  }
  const binary = await readFile(
    resolve(
      target,
      "node_modules/onnxruntime-node/bin/napi-v6/win32/x64/onnxruntime.dll",
    ),
  );
  await writeFile(
    resolve(target, "runtime.json"),
    JSON.stringify(
      {
        versions,
        platform: "win32-x64",
        provider: "cpu",
        onnxruntimeDllSha256: createHash("sha256").update(binary).digest("hex"),
        modelBundled: false,
      },
      null,
      2,
    ) + "\n",
  );
}
