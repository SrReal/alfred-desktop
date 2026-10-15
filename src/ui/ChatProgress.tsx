import { useEffect, useState } from "react";
import { on, request } from "../api";

export interface ProgressStep {
  id: string;
  title?: string;
  tool?: string;
  status: string;
  created_at?: string;
  updated_at?: string;
}
export interface ChatRun {
  id: string;
  conversation_id: string;
  message_id: string;
  status: string;
  revision: number;
  created_at: string;
  updated_at: string;
  plan: ProgressStep[];
  actions: ProgressStep[];
}
export function mergeProgress(rows: ChatRun[], incoming: ChatRun[]): ChatRun[] {
  const merged = new Map(rows.map((row) => [row.id, row]));
  let changed = false;
  for (const row of incoming) {
    const current = merged.get(row.id);
    if (!current || row.revision > current.revision) {
      merged.set(row.id, row);
      changed = true;
    }
  }
  if (!changed) return rows;
  return [...merged.values()].sort((a, b) =>
    a.created_at.localeCompare(b.created_at),
  );
}
export function useChatProgress(conversation: string) {
  const [state, setState] = useState<{
    id: string;
    runs: ChatRun[];
    error: string;
  }>({ id: "", runs: [], error: "" });
  useEffect(() => {
    let live = true;
    let off: (() => void) | undefined;
    setState({ id: conversation, runs: [], error: "" });
    if (!conversation) return;
    const accept = (rows: ChatRun[]) => {
      if (live)
        setState((old) => ({
          id: conversation,
          runs: mergeProgress(
            old.id === conversation ? old.runs : [],
            rows.filter((r) => r.conversation_id === conversation),
          ),
          error: "",
        }));
    };
    // Subscribe before the snapshot; revision merging keeps late snapshots from
    // replacing newer events. A changed conversation invalidates both callbacks.
    void on<ChatRun>("alfredpet:progress", (row) => {
      if (row.conversation_id === conversation) accept([row]);
    })
      .then(async (unsubscribe) => {
        if (!live) {
          unsubscribe();
          return;
        }
        off = unsubscribe;
        accept(await request<ChatRun[]>("chat.progress", { id: conversation }));
      })
      .catch(() => {
        if (live)
          setState((old) => ({
            ...old,
            error:
              "No se pudo cargar el progreso. Vuelve a abrir esta conversación para reintentarlo.",
          }));
      });
    return () => {
      live = false;
      off?.();
    };
  }, [conversation]);
  return state.id === conversation
    ? state
    : { id: conversation, runs: [], error: "" };
}
const states: Record<string, string> = {
  pending: "Pendiente",
  in_progress: "En curso",
  running: "En curso",
  completed: "Completado",
  waiting_permission: "Esperando autorización",
  waiting_user: "Esperando tu respuesta",
  denied: "Autorización denegada o caducada",
  error: "Error",
  interrupted: "Interrumpido",
  blocked: "Bloqueado",
  skipped: "Omitido",
};
export function runStatus(run: ChatRun): string {
  if (run.status === "running") {
    if (run.actions.some((a) => a.status === "waiting_permission"))
      return "Esperando autorización";
    if (run.actions.some((a) => a.status === "waiting_user"))
      return "Esperando tu respuesta";
    if (run.actions.some((a) => a.status === "running"))
      return "Ejecutando acciones";
    return "Alfred está trabajando";
  }
  if (run.status !== "completed") return states[run.status] ?? run.status;
  if (run.actions.some((a) => a.status !== "completed"))
    return "Finalizado con incidencias";
  if (run.plan.some((s) => !["completed", "skipped"].includes(s.status)))
    return "Finalizado con pasos pendientes";
  return "Finalizado";
}
function Step({ step }: { step: ProgressStep }) {
  return (
    <li className={`progress-step progress-${step.status}`}>
      <span className="progress-mark" aria-hidden="true">
        {step.status === "completed"
          ? "✓"
          : ["error", "denied", "blocked"].includes(step.status)
            ? "!"
            : "·"}
      </span>
      <span className="progress-step-title">
        {step.title ?? step.tool}
        <small>{states[step.status] ?? step.status}</small>
      </span>
      {step.updated_at && (
        <time
          dateTime={step.updated_at}
          title={new Date(step.updated_at).toLocaleString()}
        >
          {new Date(step.updated_at).toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          })}
        </time>
      )}
    </li>
  );
}
export function ChatProgress({ run }: { run: ChatRun }) {
  const [expanded, setExpanded] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  if (!run.plan.length && !run.actions.length) return null;
  const open = expanded ?? run.status === "running";
  const waiting =
    run.status === "running" &&
    run.actions.some((a) =>
      ["waiting_permission", "waiting_user"].includes(a.status),
    );
  const completed = run.actions.filter((a) => a.status === "completed").length;
  return (
    <section className="chat-progress" aria-label="Plan y progreso">
      <button
        type="button"
        className="progress-heading"
        aria-expanded={open}
        aria-controls={`progress-${run.id}`}
        onClick={() => setExpanded(!open)}
      >
        <span aria-hidden="true">{open ? "▾" : "▸"}</span>
        <span>
          <strong>Plan y progreso</strong>
          <small>
            {runStatus(run)}
            {run.actions.length > 0 &&
              ` · ${completed}/${run.actions.length} acciones completadas`}
          </small>
        </span>
      </button>
      {open && (
        <div id={`progress-${run.id}`} className="progress-body">
          {run.plan.length > 0 && (
            <>
              <h4>Plan de Alfred</h4>
              <ol>
                {run.plan.map((s) => (
                  <Step key={s.id} step={s} />
                ))}
              </ol>
              <p className="progress-note">
                Pasos actualizados por Alfred. Las acciones registradas aparecen
                debajo.
              </p>
            </>
          )}
          {run.actions.length > 0 && (
            <>
              <h4>Acciones registradas</h4>
              <ol>
                {run.actions.map((s) => (
                  <Step key={s.id} step={s} />
                ))}
              </ol>
            </>
          )}
          {waiting && (
            <button
              type="button"
              onClick={() => {
                setError("");
                void request("window.show", { surface: "permission" }).catch(
                  () =>
                    setError("No se pudo abrir la ventana de autorización."),
                );
              }}
            >
              Revisar solicitud
            </button>
          )}
          {error && <p role="alert">{error}</p>}
        </div>
      )}
    </section>
  );
}
