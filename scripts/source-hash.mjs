import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
export async function sourceHash(root) {
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
          (p) =>
            /^(src\/|native\/|scripts\/|tests\/|sidecars\/|assets\/|src-tauri\/)/.test(
              p,
            ) ||
            /^(package(-lock)?\.json|tsconfig\.json|vite\.config\.ts|eslint\.config\.js|index\.html)$/.test(
              p,
            ),
        ),
    ),
  ].sort();
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file + "\0");
    hash.update(await readFile(resolve(root, file)));
    hash.update("\0");
  }
  return hash.digest("hex");
}
