import { resolve, dirname } from "node:path";
import react from "@vitejs/plugin-react";
const root = resolve(import.meta.dirname, "../..");
export default {
  root,
  plugins: [
    {
      name: "dictation-fixture-api",
      enforce: "pre",
      resolveId(id, importer) {
        if (
          importer &&
          resolve(dirname(importer), id).replace(/\.ts$/, "") ===
            resolve(root, "src/api")
        )
          return resolve(root, "tests/fixtures/dictation-preview-api.js");
      },
    },
    react(),
  ],
  server: { host: "127.0.0.1", strictPort: true },
};
