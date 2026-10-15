import { useCallback, useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";
import { MacroRun } from "./MacroRun";

type Routine = {
  id: string;
  revision: string;
  name: string;
  macroId: string;
  body: Record<string, Json>;
  available: boolean;
  issue?: string;
};
type Run = {
  id: string;
  name: string;
  status: string;
  startedAt: string;
  finishedAt?: string;
  succeeded?: number;
  failed?: number;
  skipped?: number;
  message?: string;
};
const labels: Record<string, string> = {
  running: "En curso o esperando autorización…",
  complete: "Completada",
  incomplete: "Terminada con errores",
  interrupted: "Interrumpida",
  uncertain: "Resultado incierto",
};
export function Routines({ query }: { query: string }) {
  const [rows, setRows] = useState<Routine[]>([]);
  const [run, setRun] = useState<Run | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<Routine>();
  const [removing, setRemoving] = useState<Routine>();
  const epoch = useRef(0);
  const mounted = useRef(false);
  const pending = useRef(false);
  const refresh = useCallback(async () => {
    const generation = ++epoch.current;
    try {
      const [values, state] = await Promise.all([
        request<Routine[]>("routines.list"),
        request<Run | null>("routines.status"),
      ]);
      if (!mounted.current || generation !== epoch.current) return;
      setRows(values);
      setRun(state);
      setError("");
    } catch (e) {
      if (mounted.current && generation === epoch.current) setError(String(e));
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    let dispose: (() => void)[] = [];
    void Promise.all(
      [
        "alfredpet:routines",
        "alfredpet:macros",
        "alfredpet:modules",
        "alfredpet:settings",
      ].map((event) => on(event, () => void refresh())),
    ).then((listeners) => {
      if (mounted.current) dispose = listeners;
      else listeners.forEach((f) => f());
    });
    void refresh();
    return () => {
      mounted.current = false;
      ++epoch.current;
      dispose.forEach((f) => f());
    };
  }, [refresh]);
  async function act(r: Routine, operation: "run" | "delete") {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      await request(`routines.${operation}`, {
        id: r.id,
        revision: r.revision,
        ...(operation === "delete" ? { confirmed: true } : {}),
      });
      if (mounted.current) {
        setRemoving(undefined);
        await refresh();
      }
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  const locked = busy || run?.status === "running";
  return (
    <section className="routine-panel" aria-label="Rutinas con un botón">
      <div className="row">
        <h2>Rutinas con un botón</h2>
        <button disabled={busy} onClick={() => void refresh()}>
          Actualizar rutinas
        </button>
      </div>
      <p>
        Guarda una macro revisada con sus parámetros desde la lista de Macros.
        Cada botón solicita su ejecución con los permisos habituales.
      </p>
      {error && <p role="alert">{error}</p>}
      {run && (
        <div className="routine-status" role="status">
          <strong>
            {run.name} · {labels[run.status] ?? run.status}
          </strong>
          <span>Inicio: {new Date(run.startedAt).toLocaleString()}</span>
          {run.finishedAt && (
            <span>Fin: {new Date(run.finishedAt).toLocaleString()}</span>
          )}
          {run.succeeded !== undefined && (
            <span>
              {run.succeeded} pasos completados · {run.failed} fallidos ·{" "}
              {run.skipped} omitidos
            </span>
          )}
          {run.message && <p>{run.message}</p>}
          {["interrupted", "uncertain", "incomplete"].includes(run.status) && (
            <p>
              Revisa Actividad antes de repetir: algunos pasos pueden haberse
              completado. No se repiten ni deshacen automáticamente.
            </p>
          )}
        </div>
      )}
      {!rows.length && (
        <p>
          No hay rutinas guardadas. Elige una macro y pulsa Guardar como rutina.
        </p>
      )}
      <div className="routine-grid">
        {rows
          .filter((r) =>
            r.name.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
          )
          .map((r) => (
            <article key={r.id} className="card">
              <h3>{r.name}</h3>
              {!r.available && (
                <p>Necesita revisión o activación de sus módulos. {r.issue}</p>
              )}
              <details>
                <summary>Valores guardados</summary>
                <pre>{JSON.stringify(r.body, null, 2)}</pre>
              </details>
              <div className="row">
                <button
                  aria-label={`Ejecutar rutina ${r.name}`}
                  disabled={locked || !r.available}
                  onClick={() => void act(r, "run")}
                >
                  Ejecutar
                </button>
                <button
                  aria-label={`Revisar rutina ${r.name}`}
                  disabled={locked}
                  onClick={() => {
                    setEditing(r);
                    setRemoving(undefined);
                  }}
                >
                  Revisar
                </button>
                <button
                  aria-label={`Quitar rutina ${r.name}`}
                  disabled={locked}
                  onClick={() => {
                    setRemoving(r);
                    setEditing(undefined);
                  }}
                >
                  Quitar
                </button>
              </div>
            </article>
          ))}
      </div>
      {removing && (
        <div role="group" aria-label="Quitar rutina">
          <p>
            ¿Quitar «{removing.name}» de las rutinas? La macro original se
            conserva.
          </p>
          <button
            disabled={locked}
            onClick={() => void act(removing, "delete")}
          >
            Confirmar quitar rutina
          </button>
          <button disabled={locked} onClick={() => setRemoving(undefined)}>
            Cancelar
          </button>
        </div>
      )}
      {editing && !locked && (
        <MacroRun
          key={editing.id}
          id={editing.macroId}
          initialRoutine={editing}
          onClose={() => setEditing(undefined)}
          onResult={() => void refresh()}
          onSavedRoutine={() => {
            setEditing(undefined);
            void refresh();
          }}
        />
      )}
    </section>
  );
}
