import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";
import { CaptureAsset } from "./ManagedAssets";
import { PathPicker, PathsPicker } from "./PathPicker";
const operations = [
  ["documents.metadata", "Metadatos de documento", {}],
  ["documents.extract_text", "Extraer texto", {}],
  [
    "documents.render_pages",
    "Renderizar páginas PDF",
    { pages: [1], width: 1280 },
  ],
  ["documents.compare", "Comparar documentos", {}],
  ["documents.split", "Separar páginas PDF", { page: 1 }],
  ["documents.merge", "Combinar PDF", {}],
  ["documents.convert", "Convertir a TXT o Markdown", {}],
  ["documents.create_derived_text", "Crear texto derivado", {}],
  ["documents.search", "Buscar en documento", { query: "" }],
  [
    "documents.summarize",
    "Resumir con Claude",
    { instruction: "Resume en español" },
  ],
  ["images.metadata", "Metadatos de imagen", {}],
  ["images.resize", "Redimensionar imagen", { width: 800, height: 600 }],
  ["images.crop", "Recortar imagen", { x: 0, y: 0, width: 400, height: 300 }],
  ["images.convert", "Convertir imagen", {}],
  ["images.optimize", "Optimizar imagen", { quality: 85 }],
  ["images.compare", "Comparar imágenes", {}],
  ["images.screenshot_prep", "Preparar imagen para visión", { max_edge: 2048 }],
  [
    "images.contact_sheet",
    "Crear hoja de contacto",
    { columns: 4, cell_size: 256 },
  ],
  [
    "images.analyze",
    "Analizar imagen con Claude",
    { instruction: "Describe la imagen en español" },
  ],
] as const;
type Result = {
  conversation_id?: string;
  id?: string;
  assets?: { id: string }[];
  [key: string]: Json | undefined;
};
export function MediaPanel() {
  const [tool, setTool] = useState<string>(operations[0][0]);
  const [paths, setPaths] = useState<string[]>([]);
  const [destination, setDestination] = useState("");
  const [options, setOptions] = useState("{}");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result | null>(null);
  const [error, setError] = useState("");
  const epoch = useRef(0);
  const multiple = ["documents.merge", "images.contact_sheet"].includes(tool);
  const needsDestination = [
    "documents.compare",
    "documents.split",
    "documents.merge",
    "documents.convert",
    "documents.create_derived_text",
    "images.resize",
    "images.crop",
    "images.convert",
    "images.optimize",
    "images.compare",
    "images.contact_sheet",
  ].includes(tool);
  const external = ["documents.summarize", "images.analyze"].includes(tool);
  useEffect(() => {
    let alive = true;
    const stops: (() => void)[] = [];
    for (const event of ["alfredpet:settings", "alfredpet:modules"])
      void on(event, () => {
        if (alive) {
          ++epoch.current;
          setResult(null);
          setError("");
          setBusy(false);
        }
      }).then((stop) => {
        if (alive) stops.push(stop);
        else stop();
      });
    return () => {
      alive = false;
      ++epoch.current;
      stops.forEach((stop) => stop());
    };
  }, []);
  async function run() {
    const current = ++epoch.current;
    setBusy(true);
    setResult(null);
    setError("");
    try {
      const parsed: Json = JSON.parse(options);
      if (!parsed || Array.isArray(parsed) || typeof parsed !== "object")
        throw new Error("Las opciones deben ser un objeto JSON");
      if (["path", "paths", "destination"].some((key) => key in parsed))
        throw new Error("Indica los archivos en sus campos correspondientes");
      const input = {
        ...parsed,
        ...(multiple ? { paths } : { path: paths[0] }),
        ...(needsDestination ? { destination: destination.trim() } : {}),
      };
      const value = await request<Result>("tools.execute", { tool, input });
      if (epoch.current === current) setResult(value);
    } catch (e) {
      if (epoch.current === current)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (epoch.current === current) setBusy(false);
    }
  }
  const assetIds =
    result?.assets?.map((a) => a.id) ?? (result?.id ? [result.id] : []);
  const display = result
    ? Object.fromEntries(
        Object.entries(result).filter(
          ([key]) => !["assets", "thumbnail_data_url"].includes(key),
        ),
      )
    : null;
  return (
    <section className="card media-panel">
      <h1>Documentos e imágenes</h1>
      <p>
        Los derivados conservan los originales y requieren un destino nuevo. PDF
        se convierte a TXT o Markdown; los documentos escaneados requieren
        páginas de visión explícitas.
      </p>
      <label>
        Operación
        <select
          aria-label="Operación de medios"
          value={tool}
          disabled={busy}
          onChange={(e) => {
            const op = operations.find((o) => o[0] === e.target.value)!;
            ++epoch.current;
            setTool(op[0]);
            setPaths((previous) => previous.slice(0, 1));
            setDestination("");
            setOptions(JSON.stringify(op[2], null, 2));
            setResult(null);
            setError("");
          }}
        >
          {operations.map(([id, label]) => (
            <option key={id} value={id}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label>
        {multiple ? "Archivos de origen" : "Archivo de origen"}
        <PathsPicker
          key={tool}
          label="Archivos de medios"
          mode="file"
          multiple={multiple}
          value={paths}
          disabled={busy}
          onChange={setPaths}
        />
      </label>
      {needsDestination && (
        <label>
          {tool.endsWith("compare")
            ? "Archivo para comparar"
            : "Archivo de destino nuevo"}
          <PathPicker
            key={tool}
            label="Destino de medios"
            mode={tool.endsWith("compare") ? "file" : "save"}
            value={destination}
            disabled={busy}
            onChange={setDestination}
          />
        </label>
      )}
      <label>
        Opciones JSON
        <textarea
          aria-label="Opciones de medios"
          value={options}
          disabled={busy}
          onChange={(e) => setOptions(e.target.value)}
        />
      </label>
      {external && (
        <p>
          El contenido seleccionado se enviará al CLI oficial de Claude con tu
          suscripción, tras la revisión individual de permisos. Para PDF
          escaneado, indica vision_pages con las páginas que quieras analizar.
        </p>
      )}
      <button
        disabled={
          busy || !paths.length || (needsDestination && !destination.trim())
        }
        onClick={() => void run()}
      >
        {busy
          ? "Procesando…"
          : external
            ? "Revisar y enviar a Claude"
            : "Revisar y ejecutar"}
      </button>
      {error && <p role="alert">{error}</p>}
      {display && <pre>{JSON.stringify(display, null, 2)}</pre>}
      {assetIds.map((id) => (
        <CaptureAsset key={id} id={id} onDeleted={() => setResult(null)} />
      ))}
      {result?.conversation_id && (
        <button
          onClick={() =>
            void request("conversations.open", {
              id: result.conversation_id!,
            }).catch((e) => setError(String(e)))
          }
        >
          Abrir análisis en Chat
        </button>
      )}
    </section>
  );
}
