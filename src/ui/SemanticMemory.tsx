import { useCallback, useEffect, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";

type Status = {
  available: boolean;
  enabled: boolean;
  installed: boolean;
  model: string;
  bytes: number;
  dimensions: number;
  indexed: number;
  last_error: string | null;
  job: {
    status?: string;
    phase?: string;
    completed?: number;
    total?: number;
    error?: string;
  } | null;
};
type Match = {
  id: string;
  kind: string;
  title: string;
  content: string;
  semantic_score: number;
  fts_score: number;
  sources: { conversation_id: string }[];
};
export function SemanticMemory() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [consent, setConsent] = useState(false);
  const [query, setQuery] = useState("");
  const [project, setProject] = useState("");
  const [projects, setProjects] = useState<{ id: string; name: string }[]>([]);
  const [matches, setMatches] = useState<Match[]>([]);
  const reload = useCallback(async () => {
    try {
      setStatus(await request<Status>("semantic.status"));
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void reload();
    void request<{ id: string; name: string }[]>("projects.list")
      .then(setProjects)
      .catch((e) => setError(String(e)));
    let alive = true;
    let unlisten: (() => void) | undefined;
    void on("alfredpet:semantic", () => void reload())
      .then((cleanup) => {
        if (alive) unlisten = cleanup;
        else cleanup();
      })
      .catch((e) => setError(String(e)));
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [reload]);
  async function action(operation: string, input: Json = {}) {
    setBusy(true);
    setError("");
    try {
      await request(operation, input);
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  const running = status?.job?.status === "running";
  return (
    <section className="card" aria-label="Memoria semántica local">
      <h3>Memoria semántica local</h3>
      <p>
        Busca por significado en memorias y resúmenes, dentro del proyecto
        seleccionado y la memoria global. El modelo funciona en CPU. Los textos
        se procesan en este equipo; la búsqueda por palabras sigue disponible.
      </p>
      <p>
        MiniLM multilingüe · 50 idiomas · 384 dimensiones · Apache 2.0 ·
        descarga de {Math.ceil((status?.bytes ?? 127540686) / 1024 / 1024)} MiB.
      </p>
      {status && !status.available && (
        <p role="status">
          Disponible en AlfredPet de escritorio con el componente semántico
          instalado.
        </p>
      )}
      {status?.available && (
        <>
          <p>
            {status.installed
              ? "Modelo descargado"
              : "Modelo pendiente de descarga"}{" "}
            · {status.enabled ? "Activado" : "Desactivado"} · {status.indexed}{" "}
            documentos indexados.
          </p>
          {!status.enabled && (
            <>
              <label>
                <input
                  type="checkbox"
                  checked={consent}
                  onChange={(e) => setConsent(e.target.checked)}
                />
                Autorizo descargar este modelo desde Hugging Face.
              </label>
              <button
                disabled={!consent || busy || running}
                onClick={() =>
                  void action("semantic.download", { consent: true })
                }
              >
                {status.installed ? "Reparar descarga" : "Descargar modelo"}
              </button>
            </>
          )}
          <div className="row">
            <button
              disabled={
                !status.installed || busy || (running && !status.enabled)
              }
              onClick={() =>
                void action("settings.set", {
                  key: "semantic_memory",
                  value: !status.enabled,
                })
              }
            >
              {status.enabled ? "Desactivar" : "Activar"}
            </button>
            <button
              disabled={!status.enabled || busy || running}
              onClick={() => void action("semantic.rebuild")}
            >
              Reconstruir índice semántico
            </button>
            {running && (
              <button
                disabled={busy}
                onClick={() => void action("semantic.cancel")}
              >
                Cancelar
              </button>
            )}
          </div>
          {running && (
            <div role="status">
              <p>
                {status.job?.phase === "download" ? "Descargando" : "Indexando"}
                : {status.job?.completed ?? 0} / {status.job?.total ?? "…"}
              </p>
              {!!status.job?.total && (
                <progress
                  max={status.job.total}
                  value={status.job.completed ?? 0}
                />
              )}
            </div>
          )}
          {status.job?.status && !running && (
            <p role="status">Última operación: {status.job.status}</p>
          )}
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setBusy(true);
              setError("");
              void request<Match[]>("semantic.query", {
                query,
                project_id: project || null,
              })
                .then(setMatches)
                .catch((e) => setError(String(e)))
                .finally(() => setBusy(false));
            }}
          >
            <input
              aria-label="Consulta por significado"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Lo de MariaDB del servidor"
              maxLength={4096}
            />
            <select
              aria-label="Proyecto de la búsqueda semántica"
              value={project}
              onChange={(e) => {
                setProject(e.target.value);
                setMatches([]);
              }}
            >
              <option value="">Memoria global</option>
              {projects.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
            <button disabled={busy || !query.trim()}>Buscar</button>
          </form>
          {matches.map((match) => (
            <article className="card" key={`${match.kind}:${match.id}`}>
              <strong>{match.title}</strong>
              <p>{match.content}</p>
              <small>
                {match.kind === "summary"
                  ? "Resumen de conversación"
                  : "Memoria"}{" "}
                · similitud {match.semantic_score.toFixed(3)} · palabras{" "}
                {match.fts_score.toFixed(3)}
              </small>
              {match.sources
                .filter((s) => s.conversation_id)
                .map((s, i) => (
                  <button
                    key={i}
                    onClick={() =>
                      void action("conversations.open", {
                        id: s.conversation_id,
                      })
                    }
                  >
                    Abrir origen
                  </button>
                ))}
            </article>
          ))}
        </>
      )}
      {(error || status?.job?.error || status?.last_error) && (
        <p role="alert">{error || status?.job?.error || status?.last_error}</p>
      )}
    </section>
  );
}
