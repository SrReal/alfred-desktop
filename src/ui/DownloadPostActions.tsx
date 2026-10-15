import { PathPicker } from "./PathPicker";
import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";
type Preset = {
  id: string;
  name?: string;
  fingerprint?: string;
  registered_fingerprint?: string;
  enabled?: boolean;
  health?: string;
  error_code?: string;
  definition?: Json;
};
export function DownloadPostActions() {
  const [presets, setPresets] = useState<Preset[]>([]);
  const [selected, setSelected] = useState("");
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [steps, setSteps] = useState(
    '[{"tool":"downloads.hash","input":{}},{"tool":"downloads.classify","input":{}}]',
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<Json | null>(null);
  const generation = useRef(0);
  const active = useRef(false);
  const mounted = useRef(false);
  useEffect(() => {
    let alive = true;
    mounted.current = true;
    const stops: (() => void)[] = [];
    async function reload() {
      const epoch = ++generation.current;
      setResult(null);
      setError("");
      setBusy(active.current);
      try {
        const rows = await request<Preset[]>("tools.execute", {
          tool: "downloads.post_actions_list",
          input: {},
        });
        if (alive && epoch === generation.current) {
          setPresets(rows);
          setSelected("");
        }
      } catch (e) {
        if (alive && epoch === generation.current) {
          setPresets([]);
          setSelected("");
          setError(String(e));
        }
      }
    }
    void reload();
    for (const event of [
      "alfredpet:download_post_actions",
      "alfredpet:macros",
      "alfredpet:modules",
      "alfredpet:settings",
    ])
      void on(event, reload).then((stop) => {
        if (alive) stops.push(stop);
        else stop();
      });
    return () => {
      alive = false;
      mounted.current = false;
      ++generation.current;
      stops.forEach((stop) => stop());
    };
  }, []);
  async function execute(save: boolean) {
    const epoch = ++generation.current;
    if (active.current) return;
    active.current = true;
    setBusy(true);
    setError("");
    setResult(null);
    try {
      let input: Json;
      const tool = save
        ? "downloads.post_actions_save"
        : "downloads.post_actions_run";
      if (save) {
        const parsed: Json = JSON.parse(steps);
        if (!Array.isArray(parsed) || !parsed.length || parsed.length > 10)
          throw new Error("Indica de uno a diez pasos JSON");
        input = { name: name.trim(), steps: parsed };
      } else {
        const row = presets.find((p) => p.id === selected);
        if (
          !row?.fingerprint ||
          !row.enabled ||
          row.fingerprint !== row.registered_fingerprint
        )
          throw new Error(
            "El preset cambió o está deshabilitado; vuelve a revisarlo",
          );
        input = {
          id: row.id,
          path: path.trim(),
          expected: { fingerprint: row.fingerprint },
        };
      }
      const value = await request<Json>("tools.execute", { tool, input });
      if (epoch === generation.current) {
        setResult(value);
        if (save) {
          const rows = await request<Preset[]>("tools.execute", {
            tool: "downloads.post_actions_list",
            input: {},
          });
          if (epoch === generation.current) setPresets(rows);
        }
      }
    } catch (e) {
      if (epoch === generation.current)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      active.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  const preset = presets.find((p) => p.id === selected);
  let editableSteps: Json[] = [];
  try {
    const parsed: Json = JSON.parse(steps);
    if (Array.isArray(parsed)) editableSteps = parsed;
  } catch {
    /* The JSON editor reports invalid drafts when saving. */
  }
  return (
    <section>
      <h2>Acciones posteriores a una descarga</h2>
      <p>
        Los presets se ejecutan manualmente sobre un archivo terminado. Cada
        macro y acción conserva sus permisos. Se mantiene el original y no se
        ejecuta el archivo descargado.
      </p>
      <label>
        Preset de descarga
        <select
          aria-label="Preset de descarga"
          value={selected}
          onChange={(e) => {
            setSelected(e.target.value);
            setResult(null);
          }}
          disabled={busy}
        >
          <option value="">Seleccionar preset</option>
          {presets.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name ?? p.id}
              {p.health ? ` · ${p.health}` : ""}
            </option>
          ))}
        </select>
      </label>
      {preset && (
        <pre>
          {JSON.stringify(
            preset.definition ?? { error: preset.error_code },
            null,
            2,
          )}
        </pre>
      )}
      <label>
        Archivo descargado
        <PathPicker
          label="Archivo descargado"
          mode="file"
          value={path}
          disabled={busy}
          onChange={setPath}
        />
      </label>
      <button
        disabled={
          busy ||
          !selected ||
          !path.trim() ||
          !preset?.enabled ||
          preset.fingerprint !== preset.registered_fingerprint
        }
        onClick={() => void execute(false)}
      >
        Revisar acciones de descarga
      </button>
      <details>
        <summary>Crear preset de descarga</summary>
        <label>
          Nombre del preset
          <input
            aria-label="Nombre del preset"
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={busy}
          />
        </label>
        <p>
          Pasos disponibles: hash, clasificación, copia, extracción ZIP y
          lecturas de documentos o imágenes. El archivo de origen se elige al
          ejecutar; las copias y extracciones requieren un destino nuevo.
        </p>
        <label>
          Pasos del preset
          <textarea
            aria-label="Pasos del preset"
            value={steps}
            onChange={(e) => setSteps(e.target.value)}
            disabled={busy}
          />
        </label>
        {editableSteps.map((step, index) => {
          if (
            !step ||
            typeof step !== "object" ||
            Array.isArray(step) ||
            !["files.copy", "files.extract"].includes(String(step.tool)) ||
            !step.input ||
            typeof step.input !== "object" ||
            Array.isArray(step.input)
          )
            return null;
          return (
            <PathPicker
              key={`${index}:${step.tool}`}
              label={`Destino nuevo del paso ${index + 1}`}
              mode="save"
              disabled={busy}
              value={
                typeof step.input.destination === "string"
                  ? step.input.destination
                  : ""
              }
              onChange={(destination) =>
                setSteps(
                  JSON.stringify(
                    editableSteps.map((item, i) =>
                      i === index
                        ? {
                            ...step,
                            input: {
                              ...(step.input as Record<string, Json>),
                              destination,
                            },
                          }
                        : item,
                    ),
                    null,
                    2,
                  ),
                )
              }
            />
          );
        })}
        <button
          disabled={busy || !name.trim()}
          onClick={() => void execute(true)}
        >
          Revisar y guardar preset
        </button>
      </details>
      {error && <p role="alert">{error}</p>}
      {result !== null && (
        <pre aria-label="Resultado de acciones de descarga">
          {JSON.stringify(result, null, 2)}
        </pre>
      )}
    </section>
  );
}
