import { useCallback, useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import type { Conversation, Json, Project, Task } from "../types";
type Draft = {
  id: string | null;
  revision: number;
  title: string;
  description: string;
  due: string;
  frequency: "none" | "daily" | "weekly" | "monthly";
  interval: number;
  project: string;
  conversation: string;
};
type Run = {
  id: string;
  status: string;
  ended_at: string;
  scheduled_for: string | null;
  error: string | null;
};
const blank = (): Draft => ({
  id: null,
  revision: 0,
  title: "",
  description: "",
  due: "",
  frequency: "none",
  interval: 1,
  project: "",
  conversation: "",
});
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
function localDate(value: string | null) {
  if (!value) return "";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "";
  return `${date.getFullYear().toString().padStart(4, "0")}-${(date.getMonth() + 1).toString().padStart(2, "0")}-${date.getDate().toString().padStart(2, "0")}T${date.getHours().toString().padStart(2, "0")}:${date.getMinutes().toString().padStart(2, "0")}:${date.getSeconds().toString().padStart(2, "0")}.${date.getMilliseconds().toString().padStart(3, "0")}`;
}
function displayDate(value: string | null) {
  if (!value) return "Sin fecha";
  const date = new Date(value);
  return Number.isFinite(date.getTime())
    ? date.toLocaleString()
    : `Fecha inválida: ${value}`;
}
async function tool<T>(action: string, input: Json = {}) {
  return request<T>("tools.execute", { tool: `tasks.${action}`, input });
}
export function TasksPanel() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [draft, setDraft] = useState<Draft>(blank);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [error, setError] = useState("");
  const [runs, setRuns] = useState<{
    id: string;
    title: string;
    rows: Run[];
    more: boolean;
  } | null>(null);
  const live = useRef(false);
  const generation = useRef(0);
  const working = useRef(false);
  const historyGeneration = useRef(0);
  const reload = useCallback(async () => {
    const current = ++generation.current;
    setLoading(true);
    try {
      const [rows, p, c] = await Promise.all([
        tool<Task[]>("list"),
        request<Project[]>("projects.list"),
        request<Conversation[]>("conversations.list"),
      ]);
      if (!live.current || generation.current !== current) return;
      if (![rows, p, c].every(Array.isArray))
        throw new Error("TASK_LIST_RESPONSE");
      setTasks(rows);
      setProjects(p);
      setConversations(c);
      setHasMore(rows.length === 50);
    } catch (error) {
      if (live.current && generation.current === current)
        setError(message(error));
    } finally {
      if (live.current && generation.current === current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    live.current = true;
    void reload();
    let stop: (() => void) | undefined;
    void on("alfredpet:tasks", () => void reload())
      .then((dispose) => {
        if (live.current) stop = dispose;
        else dispose();
      })
      .catch((error) => {
        if (live.current) setError(message(error));
      });
    return () => {
      live.current = false;
      generation.current++;
      historyGeneration.current++;
      stop?.();
    };
  }, [reload]);
  async function change(action: string, input: Json, reset = false) {
    if (working.current) return;
    working.current = true;
    setBusy(true);
    setError("");
    try {
      await tool(action, input);
      if (!live.current) return;
      if (reset) setDraft(blank());
      await reload();
    } catch (error) {
      if (live.current) setError(message(error));
    } finally {
      working.current = false;
      if (live.current) setBusy(false);
    }
  }
  function edit(task: Task) {
    setError("");
    setDraft({
      id: task.id,
      revision: task.revision,
      title: task.title,
      description: task.description,
      due: localDate(task.due_at),
      frequency: task.recurrence?.frequency ?? "none",
      interval: task.recurrence?.interval ?? 1,
      project: task.project_id ?? "",
      conversation: task.conversation_id ?? "",
    });
  }
  async function save() {
    try {
      const input = {
        title: draft.title.trim(),
        description: draft.description,
        due_at: draft.due ? new Date(draft.due).toISOString() : null,
        recurrence:
          draft.frequency === "none"
            ? null
            : { frequency: draft.frequency, interval: draft.interval },
        project_id: draft.project || null,
        conversation_id: draft.conversation || null,
        ...(draft.id
          ? { id: draft.id, expected_revision: draft.revision }
          : {}),
      };
      await change(draft.id ? "update" : "create", input, true);
    } catch (error) {
      setError(message(error));
    }
  }
  async function more() {
    if (loading || busy) return;
    const current = ++generation.current;
    setLoading(true);
    try {
      const rows = await tool<Task[]>("list", { offset: tasks.length });
      if (!live.current || generation.current !== current) return;
      if (!Array.isArray(rows)) throw new Error("TASK_LIST_RESPONSE");
      setTasks((existing) => [
        ...existing,
        ...rows.filter((row) => !existing.some((task) => task.id === row.id)),
      ]);
      setHasMore(rows.length === 50);
    } catch (error) {
      if (live.current) setError(message(error));
    } finally {
      if (live.current && generation.current === current) setLoading(false);
    }
  }
  async function history(id: string, title: string, append = false) {
    const current = ++historyGeneration.current;
    try {
      const rows = await tool<Run[]>(
        "history",
        append && runs ? { id, offset: runs.rows.length } : { id },
      );
      if (!live.current || historyGeneration.current !== current) return;
      if (!Array.isArray(rows)) throw new Error("TASK_HISTORY_RESPONSE");
      setRuns({
        id,
        title,
        rows: append && runs ? [...runs.rows, ...rows] : rows,
        more: rows.length === 100,
      });
    } catch (error) {
      if (live.current) setError(message(error));
    }
  }
  const stale =
    draft.id &&
    tasks.some(
      (task) => task.id === draft.id && task.revision !== draft.revision,
    );
  return (
    <section aria-label="Tareas y recordatorios">
      <h1>Tareas y recordatorios</h1>
      <form
        className="card task-form"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <h2>{draft.id ? "Editar tarea" : "Nueva tarea"}</h2>
        <div className="task-form-grid">
          <label className="task-form-wide">
            Título
            <input
              aria-label="Nueva tarea"
              maxLength={500}
              value={draft.title}
              onChange={(event) =>
                setDraft({ ...draft, title: event.target.value })
              }
              disabled={busy}
            />
          </label>
          <label className="task-form-wide">
            Descripción
            <textarea
              aria-label="Descripción de tarea"
              rows={3}
              maxLength={8000}
              value={draft.description}
              onChange={(event) =>
                setDraft({ ...draft, description: event.target.value })
              }
              disabled={busy}
            />
          </label>
          <label>
            Fecha del recordatorio
            <input
              type="datetime-local"
              step="0.001"
              aria-label="Fecha del recordatorio"
              value={draft.due}
              onChange={(event) =>
                setDraft({ ...draft, due: event.target.value })
              }
              disabled={busy}
            />
          </label>
          <div className="task-repeat">
            <label>
              Repetición
              <select
                aria-label="Repetición de tarea"
                value={draft.frequency}
                onChange={(event) =>
                  setDraft({
                    ...draft,
                    frequency: event.target.value as Draft["frequency"],
                  })
                }
                disabled={busy}
              >
                <option value="none">Sin repetición</option>
                <option value="daily">Diaria</option>
                <option value="weekly">Semanal</option>
                <option value="monthly">Mensual</option>
              </select>
            </label>
            {draft.frequency !== "none" && (
              <label>
                Intervalo
                <input
                  aria-label="Intervalo de repetición"
                  type="number"
                  min={1}
                  max={365}
                  value={draft.interval}
                  onChange={(event) =>
                    setDraft({ ...draft, interval: Number(event.target.value) })
                  }
                  disabled={busy}
                />
              </label>
            )}
          </div>
          {draft.frequency !== "none" && (
            <details className="task-form-wide task-form-hint">
              <summary>Cómo se repite el recordatorio</summary>
              <p>
                La fecha se muestra en tu hora local. La repetición mantiene su
                hora UTC; puede cambiar la hora local al cambiar el horario de
                verano. Completar programa el siguiente vencimiento futuro, sin
                acumular avisos atrasados. La repetición mensual conserva el día
                original y usa el último día en meses más cortos.
              </p>
            </details>
          )}
          <label>
            Proyecto
            <select
              aria-label="Proyecto de tarea"
              value={draft.project}
              onChange={(event) =>
                setDraft({ ...draft, project: event.target.value })
              }
              disabled={busy}
            >
              <option value="">Sin proyecto</option>
              {draft.project &&
                !projects.some((p) => p.id === draft.project) && (
                  <option value={draft.project}>
                    Vínculo a proyecto inexistente
                  </option>
                )}
              {projects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            Conversación
            <select
              aria-label="Conversación de tarea"
              value={draft.conversation}
              onChange={(event) =>
                setDraft({ ...draft, conversation: event.target.value })
              }
              disabled={busy}
            >
              <option value="">Sin conversación</option>
              {draft.conversation &&
                !conversations.some((c) => c.id === draft.conversation) && (
                  <option value={draft.conversation}>
                    Vínculo a conversación inexistente
                  </option>
                )}
              {conversations.map((conversation) => (
                <option key={conversation.id} value={conversation.id}>
                  {conversation.title}
                </option>
              ))}
            </select>
          </label>
        </div>
        {stale && (
          <p role="status">
            La tarea cambió mientras la editabas. Vuelve a pulsar Editar para
            revisar el estado actual.
          </p>
        )}
        <div className="row task-form-actions">
          <button
            className="primary"
            type="submit"
            disabled={
              busy ||
              loading ||
              !!stale ||
              !draft.title.trim() ||
              (draft.frequency !== "none" &&
                (!draft.due ||
                  !Number.isInteger(draft.interval) ||
                  draft.interval < 1 ||
                  draft.interval > 365))
            }
          >
            {busy
              ? "Esperando resultado…"
              : draft.id
                ? "Guardar tarea"
                : "Crear"}
          </button>
          {draft.id && (
            <button
              type="button"
              disabled={busy}
              onClick={() => setDraft(blank())}
            >
              Cancelar edición
            </button>
          )}
        </div>
      </form>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <button disabled={busy || loading} onClick={() => void reload()}>
        Actualizar tareas
      </button>
      {tasks.map((task) => (
        <article className="card" key={task.id}>
          <strong>{task.title}</strong>
          <span className="badge">{task.status}</span>
          <p>{task.description}</p>
          <p>{displayDate(task.due_at)}</p>
          {task.recurrence && (
            <p>
              Repetición{" "}
              {
                { daily: "diaria", weekly: "semanal", monthly: "mensual" }[
                  task.recurrence.frequency
                ]
              }{" "}
              · intervalo {task.recurrence.interval} · UTC
            </p>
          )}
          {task.recurrence_error && (
            <p className="error">
              {task.recurrence_error} · Edita la repetición para corregirla.
            </p>
          )}
          {task.project_id && (
            <p>Proyecto: {task.project_name ?? "Vínculo inexistente"}</p>
          )}
          {task.conversation_id && (
            <button
              disabled={!task.conversation_title || busy}
              onClick={() =>
                void request("conversations.open", {
                  id: task.conversation_id,
                }).catch((error) => setError(message(error)))
              }
            >
              Abrir conversación:{" "}
              {task.conversation_title ?? "Vínculo inexistente"}
            </button>
          )}
          <div className="row">
            <button disabled={busy || loading} onClick={() => edit(task)}>
              Editar
            </button>
            <button
              disabled={busy || loading || task.status === "succeeded"}
              onClick={() =>
                void change("complete", {
                  id: task.id,
                  expected_revision: task.revision,
                })
              }
            >
              {task.recurrence
                ? "Completar y programar siguiente"
                : "Completar"}
            </button>
            {task.status === "succeeded" && (
              <button
                disabled={busy || loading}
                onClick={() =>
                  void change("update", {
                    id: task.id,
                    expected_revision: task.revision,
                    reopen: true,
                  })
                }
              >
                Reabrir
              </button>
            )}
            <button
              disabled={busy || loading}
              onClick={() =>
                void change(
                  "delete",
                  { id: task.id, expected_revision: task.revision },
                  draft.id === task.id,
                )
              }
            >
              Borrar
            </button>
            <button
              disabled={busy}
              onClick={() => void history(task.id, task.title)}
            >
              Ver historial
            </button>
          </div>
        </article>
      ))}
      {!tasks.length && !loading && <p className="empty">No hay tareas.</p>}
      {hasMore && (
        <button disabled={busy || loading} onClick={() => void more()}>
          Cargar más tareas
        </button>
      )}
      {runs && (
        <section className="card" aria-label="Historial de tarea">
          <h2>Historial: {runs.title}</h2>
          {!runs.rows.length && <p>Sin ejecuciones registradas.</p>}
          {runs.rows.map((run) => (
            <p key={run.id}>
              {run.status} · {displayDate(run.ended_at)} · vencimiento:{" "}
              {displayDate(run.scheduled_for)} {run.error}
            </p>
          ))}
          {runs.more && (
            <button onClick={() => void history(runs.id, runs.title, true)}>
              Cargar más historial
            </button>
          )}
          <button
            onClick={() => {
              historyGeneration.current++;
              setRuns(null);
            }}
          >
            Cerrar historial
          </button>
        </section>
      )}
    </section>
  );
}
