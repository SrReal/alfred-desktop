import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve, sep } from "node:path";
import http from "node:http";
import https from "node:https";
import net from "node:net";

// This worker receives local text only over stdin. It has no download operation.
const denyNetwork = () => {
  throw new Error("SEMANTIC_NETWORK_DENIED");
};
globalThis.fetch = denyNetwork;
http.request = http.get = https.request = https.get = denyNetwork;
net.Socket.prototype.connect = denyNetwork;
const { Tokenizer } = await import("@huggingface/tokenizers");
const ort = await import("onnxruntime-node");
async function hashFile(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}
const [modelPath, manifestPath] = process.argv.slice(2);
let session;
let timer;
async function initialize() {
  if (!modelPath || !manifestPath) throw new Error("SEMANTIC_MODEL_REQUIRED");
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  const root = resolve(modelPath);
  if (
    manifest.dimensions !== 384 ||
    manifest.max_tokens !== 128 ||
    manifest.files.length !== 6
  )
    throw new Error("SEMANTIC_MANIFEST_INVALID");
  for (const file of manifest.files) {
    const path = resolve(root, file.path);
    if (!path.startsWith(root + sep) || file.path.includes(".."))
      throw new Error("SEMANTIC_MANIFEST_INVALID");
    if (
      (await stat(path)).size !== file.bytes ||
      (await hashFile(path)) !== file.sha256
    )
      throw new Error("SEMANTIC_MODEL_HASH_MISMATCH");
  }
  const tokenizerConfig = JSON.parse(
    await readFile(resolve(root, "tokenizer_config.json"), "utf8"),
  );
  const tokenizer = new Tokenizer(
    JSON.parse(await readFile(resolve(root, "tokenizer.json"), "utf8")),
    tokenizerConfig,
  );
  const pad = tokenizer.token_to_id(tokenizerConfig.pad_token);
  if (!Number.isInteger(pad)) throw new Error("SEMANTIC_TOKENIZER_INVALID");
  session = await ort.InferenceSession.create(
    resolve(root, "onnx/model.onnx"),
    {
      executionProviders: ["cpu"],
      intraOpNumThreads: 2,
      interOpNumThreads: 1,
      graphOptimizationLevel: "all",
      logSeverityLevel: 3,
    },
  );
  async function embed(texts) {
    if (
      !Array.isArray(texts) ||
      texts.length < 1 ||
      texts.length > 8 ||
      texts.some(
        (t) =>
          typeof t !== "string" || t.length < 1 || Array.from(t).length > 4096,
      )
    )
      throw new Error("SEMANTIC_INPUT_LIMIT");
    // Same right truncation/padding as Transformers.js, limited to the model's
    // SentenceTransformer sequence budget. JSON contains the postprocessor.
    const encoded = texts.map((text) =>
      tokenizer.encode(text, {
        add_special_tokens: true,
        return_token_type_ids: true,
      }),
    );
    const length = Math.min(
      128,
      Math.max(...encoded.map((row) => row.ids.length)),
    );
    const ids = [],
      masks = [],
      types = [];
    for (const row of encoded)
      for (let n = 0; n < length; n++) {
        ids.push(BigInt(row.ids[n] ?? pad));
        masks.push(BigInt(n < row.ids.length ? 1 : 0));
        types.push(BigInt(row.token_type_ids?.[n] ?? 0));
      }
    const dims = [texts.length, length];
    const available = {
      input_ids: new ort.Tensor("int64", BigInt64Array.from(ids), dims),
      attention_mask: new ort.Tensor("int64", BigInt64Array.from(masks), dims),
      token_type_ids: new ort.Tensor("int64", BigInt64Array.from(types), dims),
    };
    const feeds = Object.fromEntries(
      session.inputNames.map((name) => {
        if (!available[name]) throw new Error("SEMANTIC_GRAPH_INVALID");
        return [name, available[name]];
      }),
    );
    let result;
    try {
      result = await session.run(feeds);
      const output = result.last_hidden_state;
      if (!output || output.dims.join(",") !== `${texts.length},${length},384`)
        throw new Error("SEMANTIC_GRAPH_INVALID");
      return texts.map((_, batch) => {
        const vector = Array(384).fill(0);
        let count = 0;
        for (let token = 0; token < length; token++)
          if (masks[batch * length + token] === 1n) {
            count++;
            for (let d = 0; d < 384; d++)
              vector[d] += output.data[(batch * length + token) * 384 + d];
          }
        if (!count) throw new Error("SEMANTIC_VECTOR_INVALID");
        for (let d = 0; d < 384; d++) vector[d] /= count;
        const norm = Math.hypot(...vector);
        if (!Number.isFinite(norm) || norm < 1e-8)
          throw new Error("SEMANTIC_VECTOR_INVALID");
        return vector.map((value) => value / norm);
      });
    } finally {
      for (const tensor of Object.values(available)) tensor.dispose();
      for (const tensor of Object.values(result ?? {})) tensor.dispose();
    }
  }
  return { embed, revision: manifest.revision };
}
function write(value) {
  process.stdout.write(JSON.stringify(value) + "\n");
}
function idle() {
  clearTimeout(timer);
  timer = setTimeout(() => {
    void session?.release().finally(() => process.exit(0));
  }, 60000);
}
async function* boundedLines(stream) {
  let pending = Buffer.alloc(0);
  for await (const chunk of stream) {
    let offset = 0;
    for (;;) {
      const end = chunk.indexOf(10, offset);
      const segment = chunk.subarray(offset, end < 0 ? chunk.length : end);
      if (pending.length + segment.length > 196608)
        throw new Error("SEMANTIC_INPUT_LIMIT");
      pending = Buffer.concat([pending, segment]);
      if (end < 0) break;
      yield pending.toString("utf8");
      pending = Buffer.alloc(0);
      offset = end + 1;
    }
  }
  if (pending.length) yield pending.toString("utf8");
}
try {
  const worker = await initialize();
  write({
    ready: true,
    revision: worker.revision,
    dimensions: 384,
    runtime: ort.env.versions.node,
  });
  idle();
  for await (const line of boundedLines(process.stdin)) {
    clearTimeout(timer);
    let id;
    try {
      if (Buffer.byteLength(line) > 196608)
        throw new Error("SEMANTIC_INPUT_LIMIT");
      const request = JSON.parse(line);
      id = request.id;
      if (
        typeof id !== "string" ||
        id.length > 64 ||
        request.operation !== "embed"
      )
        throw new Error("SEMANTIC_REQUEST_INVALID");
      write({ id, ok: true, vectors: await worker.embed(request.texts) });
    } catch (error) {
      write({
        id,
        ok: false,
        error: /^SEMANTIC_/.test(error.message)
          ? error.message
          : "SEMANTIC_INFERENCE_FAILED",
      });
    }
    idle();
  }
} catch (error) {
  write({
    ready: false,
    error: /^SEMANTIC_/.test(error.message)
      ? error.message
      : "SEMANTIC_INITIALIZATION_FAILED",
  });
  process.exitCode = 1;
} finally {
  clearTimeout(timer);
  await session?.release();
}
