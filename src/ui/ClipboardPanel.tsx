import { useCallback, useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import { CaptureAsset } from "./ManagedAssets";
import type { Json } from "../types";
type Status = {
  enabled: boolean;
  module_enabled: boolean;
  privacy: boolean;
  scope_generation: number;
  entries: number;
  maximum_entries: number;
  retention_days: number;
  last_error: string | null;
};
type Expected = { sequence: number; fingerprint: string };
type Snapshot = {
  expected: Expected;
  kind: string;
  text?: string;
  has_image: boolean;
  sensitive: boolean;
  formats: { id: number; name: string }[];
};
type Entry = {
  id: string;
  text: string;
  updated_at: string;
  expires_at: string;
};
const operations = [
  ["trim", "Quitar espacios exteriores"],
  ["uppercase", "Mayúsculas"],
  ["lowercase", "Minúsculas"],
  ["normalize_whitespace", "Normalizar espacios"],
  ["unique_lines", "Quitar líneas repetidas"],
  ["sort_lines", "Ordenar líneas"],
  ["pretty_json", "Formatear JSON"],
  ["minify_json", "Compactar JSON"],
];
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const tool = <T,>(action: string, input: Json = {}) =>
  request<T>("tools.execute", { tool: `clipboard.${action}`, input });
export function ClipboardPanel() {
  const [status, setStatus] = useState<Status | null>(null),
    [snapshot, setSnapshot] = useState<Snapshot | null>(null),
    [entries, setEntries] = useState<Entry[]>([]);
  const [replacement, setReplacement] = useState(""),
    [operation, setOperation] = useState("trim"),
    [preview, setPreview] = useState<{
      text: string;
      expected: Expected;
      operation: string;
    } | null>(null);
  const [asset, setAsset] = useState(""),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [notice, setNotice] = useState(""),
    [more, setMore] = useState(false);
  const live = useRef(false),
    epoch = useRef(0),
    statusGeneration = useRef(0),
    working = useRef(false),
    scope = useRef<number | null>(null);
  const clear = useCallback(() => {
    ++epoch.current;
    setSnapshot(null);
    setPreview(null);
    setEntries([]);
    setMore(false);
    setAsset("");
    setReplacement("");
    setNotice("");
  }, []);
  const reload = useCallback(async () => {
    const gen = ++statusGeneration.current;
    try {
      const next = await request<Status>("clipboard.status");
      if (!live.current || gen !== statusGeneration.current) return;
      if (
        !next ||
        typeof next.enabled !== "boolean" ||
        typeof next.module_enabled !== "boolean" ||
        typeof next.privacy !== "boolean" ||
        !Number.isSafeInteger(next.scope_generation)
      )
        throw new Error("CLIPBOARD_STATUS_RESPONSE");
      if (scope.current !== null && scope.current !== next.scope_generation)
        clear();
      scope.current = next.scope_generation;
      setStatus(next);
    } catch (e) {
      if (live.current && gen === statusGeneration.current)
        setError(message(e));
    }
  }, [clear]);
  useEffect(() => {
    live.current = true;
    void reload();
    const stops: (() => void)[] = [];
    for (const event of [
      "alfredpet:clipboard",
      "alfredpet:settings",
      "alfredpet:modules",
    ])
      void on(event, () => {
        if (!live.current) return;
        if (event !== "alfredpet:clipboard") clear();
        void reload();
      })
        .then((stop) => {
          if (live.current) stops.push(stop);
          else stop();
        })
        .catch((e) => {
          if (live.current) setError(message(e));
        });
    return () => {
      live.current = false;
      ++epoch.current;
      ++statusGeneration.current;
      stops.forEach((stop) => stop());
    };
  }, [clear, reload]);
  const allowed = !!status?.module_enabled && !status?.privacy;
  async function act(run: () => Promise<void>) {
    if (working.current) return;
    working.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await run();
    } catch (e) {
      if (live.current) {
        setError(message(e));
        if (
          /CLIPBOARD_CHANGED|CLIPBOARD_SCOPE|PRIVACY_DENIED/.test(message(e))
        ) {
          setSnapshot(null);
          setPreview(null);
        }
      }
    } finally {
      working.current = false;
      if (live.current) setBusy(false);
    }
  }
  async function inspect() {
    const gen = epoch.current;
    const data = await tool<Snapshot>("get");
    if (live.current && gen === epoch.current) {
      setSnapshot(data);
      setPreview(null);
      setAsset("");
    }
  }
  async function history(append = false) {
    const gen = epoch.current;
    const offset = append ? entries.length : 0;
    const result = await tool<{ items: Entry[] }>("history", {
      limit: 20,
      offset,
    });
    if (!Array.isArray(result.items))
      throw new Error("CLIPBOARD_HISTORY_RESPONSE");
    if (live.current && gen === epoch.current) {
      setEntries((previous) =>
        append ? [...previous, ...result.items] : result.items,
      );
      setMore(result.items.length === 20);
    }
  }
  async function write(text: string, expected: Expected) {
    const gen = epoch.current;
    await tool("set", { text, expected });
    if (live.current && gen === epoch.current) {
      setSnapshot(null);
      setPreview(null);
      setNotice(
        "Texto escrito y comprobado. Inspecciona de nuevo para otra operación.",
      );
    }
  }
  return (
    <section>
      <h1>Portapapeles</h1>
      <p>
        El historial guarda sólo texto nuevo después de activarlo. Conserva
        hasta 100 entradas y 2 MiB durante siete días; se pausa con privacidad o
        con el módulo desactivado. Las imágenes se guardan sólo cuando lo
        solicitas.
      </p>
      <label className="toggle">
        <input
          type="checkbox"
          checked={status?.enabled ?? false}
          disabled={!status || busy}
          onChange={(e) => {
            const enabled = e.target.checked;
            void act(async () => {
              clear();
              await request("settings.set", {
                key: "clipboard_history",
                value: enabled,
              });
              await reload();
            });
          }}
        />
        Guardar historial de texto
      </label>
      {status && !status.module_enabled && (
        <button
          disabled={busy}
          onClick={() =>
            void act(async () => {
              await request("modules.toggle", {
                id: "clipboard",
                enabled: true,
              });
              await reload();
            })
          }
        >
          Activar módulo de portapapeles
        </button>
      )}
      {status?.privacy && (
        <p role="status">
          Portapapeles pausado por privacidad. Puedes solicitar borrar el
          historial guardado.
        </p>
      )}
      <p>
        {status
          ? `${status.entries} entradas guardadas · historial ${status.enabled ? "activado" : "desactivado"}`
          : "Consultando estado…"}
      </p>
      {(error || status?.last_error) && (
        <div className="error" role="alert">
          {error || status?.last_error}
        </div>
      )}
      {notice && <p role="status">{notice}</p>}
      <div className="row">
        <button disabled={busy || !allowed} onClick={() => void act(inspect)}>
          Inspeccionar portapapeles
        </button>
        <button disabled={busy} onClick={() => void act(reload)}>
          Actualizar estado
        </button>
      </div>
      {snapshot && (
        <article className="card">
          <strong>Tipo detectado: {snapshot.kind}</strong>
          <p>
            {snapshot.formats.map((f) => f.name).join(", ") || "Sin formatos"}
          </p>
          {typeof snapshot.text === "string" && (
            <pre aria-label="Texto observado">{snapshot.text}</pre>
          )}
          {snapshot.has_image && (
            <button
              disabled={busy || !allowed}
              onClick={() =>
                void act(async () => {
                  const gen = epoch.current;
                  const result = await tool<{ id: string }>("image", {
                    expected: snapshot.expected,
                  });
                  if (!result.id) throw new Error("CLIPBOARD_IMAGE_RESPONSE");
                  if (live.current && gen === epoch.current)
                    setAsset(result.id);
                })
              }
            >
              Guardar imagen como activo privado
            </button>
          )}
          <label>
            Transformación
            <select
              aria-label="Transformación de texto"
              value={operation}
              disabled={busy}
              onChange={(e) => {
                setOperation(e.target.value);
                setPreview(null);
              }}
            >
              {operations.map(([id, label]) => (
                <option value={id} key={id}>
                  {label}
                </option>
              ))}
            </select>
          </label>
          <button
            disabled={busy || !allowed || typeof snapshot.text !== "string"}
            onClick={() =>
              void act(async () => {
                const gen = epoch.current;
                const result = await tool<{ text: string; expected: Expected }>(
                  "transform",
                  { operation, expected: snapshot.expected, apply: false },
                );
                if (live.current && gen === epoch.current)
                  setPreview({ ...result, operation });
              })
            }
          >
            Previsualizar transformación
          </button>
          {preview && (
            <>
              <pre aria-label="Vista previa de transformación">
                {preview.text}
              </pre>
              <button
                disabled={busy || !allowed}
                onClick={() =>
                  void act(async () => {
                    const gen = epoch.current;
                    await tool("transform", {
                      operation: preview.operation,
                      expected: preview.expected,
                      apply: true,
                    });
                    if (live.current && gen === epoch.current) {
                      setSnapshot(null);
                      setPreview(null);
                      setNotice("Transformación aplicada y comprobada.");
                    }
                  })
                }
              >
                Aplicar transformación con revisión
              </button>
            </>
          )}
          <label>
            Texto de reemplazo
            <textarea
              aria-label="Texto de reemplazo"
              value={replacement}
              disabled={busy}
              onChange={(e) => setReplacement(e.target.value)}
            />
          </label>
          <button
            disabled={
              busy ||
              !allowed ||
              Array.from(replacement).length > 64000 ||
              replacement.includes("\0")
            }
            onClick={() =>
              void act(() => write(replacement, snapshot.expected))
            }
          >
            Escribir texto con revisión
          </button>
          <p>
            Reemplazar texto descarta los demás formatos del portapapeles.
            Requiere revisión y una observación vigente.
          </p>
        </article>
      )}
      {asset && <CaptureAsset id={asset} onDeleted={() => setAsset("")} />}
      <h2>Historial de texto</h2>
      <div className="row">
        <button
          disabled={busy || !allowed || !status?.enabled}
          onClick={() => void act(() => history())}
        >
          Mostrar historial
        </button>
        <button
          disabled={busy || !status?.module_enabled || !status.entries}
          onClick={() =>
            void act(async () => {
              await tool("history_clear");
              if (live.current) {
                clear();
                await reload();
                setNotice("Historial borrado.");
              }
            })
          }
        >
          Borrar historial con confirmación
        </button>
      </div>
      {entries.map((entry) => (
        <article className="card" key={entry.id}>
          <pre>{entry.text}</pre>
          <small>
            {new Date(entry.updated_at).toLocaleString()} · caduca{" "}
            {new Date(entry.expires_at).toLocaleString()}
          </small>
          <div className="row">
            <button
              disabled={busy || !allowed}
              onClick={() =>
                void act(async () => {
                  const gen = epoch.current;
                  const observed = await tool<Snapshot>("formats");
                  if (!live.current || gen !== epoch.current)
                    throw new Error("CLIPBOARD_SCOPE_CHANGED");
                  await write(entry.text, observed.expected);
                })
              }
            >
              Copiar esta entrada con revisión
            </button>
            <button
              disabled={busy || !status?.module_enabled}
              onClick={() =>
                void act(async () => {
                  await tool("history_delete", { id: entry.id });
                  if (live.current) {
                    setEntries((previous) =>
                      previous.filter((item) => item.id !== entry.id),
                    );
                    await reload();
                  }
                })
              }
            >
              Borrar entrada con confirmación
            </button>
          </div>
        </article>
      ))}
      {more && (
        <button
          disabled={busy || !allowed}
          onClick={() => void act(() => history(true))}
        >
          Más entradas
        </button>
      )}
    </section>
  );
}
