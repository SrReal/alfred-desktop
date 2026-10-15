import { useCallback, useEffect, useState } from "react";
import { on, request } from "../api";
import type { Conversation, Json } from "../types";
type Candidate = {
  id: string;
  conversation_id: string;
  conversation_title: string;
  project_id: string | null;
  body: {
    title: string;
    content: string;
    confidence: number;
    rationale: string;
    scope: string;
    supersedes: string | null;
    evidence: { message_id: string; quote: string }[];
  };
};
type Analysis = {
  conversation_id: string;
  title: string;
  status: string;
  error_code: string | null;
  summary_json: string;
};
type Summary = {
  conversation_summary?: string;
  task_outcome?: string;
  decisions?: string[];
  artifacts?: string[];
};
function summaryOf(raw: string): Summary {
  try {
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return {};
    const row = value as Record<string, unknown>;
    return {
      conversation_summary:
        typeof row.conversation_summary === "string"
          ? row.conversation_summary
          : "",
      task_outcome:
        typeof row.task_outcome === "string" ? row.task_outcome : "",
      decisions: Array.isArray(row.decisions)
        ? row.decisions.filter((v): v is string => typeof v === "string")
        : [],
      artifacts: Array.isArray(row.artifacts)
        ? row.artifacts.filter((v): v is string => typeof v === "string")
        : [],
    };
  } catch {
    return {};
  }
}
const statuses: Record<string, string> = {
  running: "Analizando",
  succeeded: "Analizado",
  error: "Error",
  interrupted: "Interrumpido",
  idle: "Pendiente",
};
export function MemoryAnalysis() {
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [analyses, setAnalyses] = useState<Analysis[]>([]);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [conversation, setConversation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const reload = useCallback(async () => {
    try {
      const [pending, progress, history] = await Promise.all([
        request<Candidate[]>("memory.candidates"),
        request<Analysis[]>("memory.analyses"),
        request<Conversation[]>("conversations.list"),
      ]);
      setCandidates(pending);
      setAnalyses(progress);
      setConversations(history);
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void reload();
    let alive = true,
      unlisten: (() => void) | undefined;
    void on("alfredpet:memory", () => void reload())
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
  async function action(operation: string, input: Json) {
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
  return (
    <div aria-label="Análisis de memoria">
      <h3>Memoria inferida y resúmenes</h3>
      <p>
        Los turnos completados se analizan en segundo plano. Las inferencias con
        evidencia y confianza alta se guardan; las dudas y correcciones pasan
        por esta revisión. Las conversaciones sin memoria y el modo privacidad
        bloquean el análisis.
      </p>
      <div className="row">
        <select
          aria-label="Conversación para analizar"
          value={conversation}
          onChange={(e) => setConversation(e.target.value)}
        >
          <option value="">Elige una conversación</option>
          {conversations.map((row) => (
            <option key={row.id} value={row.id} disabled={!!row.no_memory}>
              {row.title}
              {row.no_memory ? " · Sin memoria" : ""}
            </option>
          ))}
        </select>
        <button
          disabled={!conversation || busy}
          onClick={() =>
            void action("memory.extract", { conversation_id: conversation })
          }
        >
          Analizar mensajes nuevos
        </button>
        <button disabled={busy} onClick={() => void reload()}>
          Actualizar análisis
        </button>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <h4>Propuestas para revisar ({candidates.length})</h4>
      {candidates.map((row) => (
        <article className="card" key={row.id}>
          <strong>{row.body.title}</strong>
          <p>{row.body.content}</p>
          <p className="meta">
            {row.body.scope === "project" ? "Proyecto" : "Global"} · Confianza:{" "}
            {Math.round(row.body.confidence * 100)} % · {row.conversation_title}
          </p>
          {row.body.supersedes && (
            <p>
              Propone sustituir una memoria existente. Revisa la corrección
              antes de aceptarla.
            </p>
          )}
          <p>{row.body.rationale}</p>
          {row.body.evidence.map((source, index) => (
            <blockquote key={`${source.message_id}-${index}`}>
              {source.quote}
            </blockquote>
          ))}
          <div className="row">
            <button
              disabled={busy}
              onClick={() =>
                void action("memory.candidate_accept", { id: row.id })
              }
            >
              Aceptar propuesta
            </button>
            <button
              disabled={busy}
              onClick={() =>
                void action("memory.candidate_reject", { id: row.id })
              }
            >
              Rechazar propuesta
            </button>
            <button
              disabled={busy}
              onClick={() =>
                void action("conversations.open", { id: row.conversation_id })
              }
            >
              Ver origen
            </button>
          </div>
        </article>
      ))}
      <h4>Resumen por conversación</h4>
      {analyses.map((row) => {
        const summary = summaryOf(row.summary_json);
        return (
          <article className="card" key={row.conversation_id}>
            <strong>{row.title}</strong> · {statuses[row.status] ?? row.status}
            {row.error_code && <p className="error">{row.error_code}</p>}
            {summary.conversation_summary && (
              <p>{summary.conversation_summary}</p>
            )}
            {summary.task_outcome && <p>Resultado: {summary.task_outcome}</p>}
            {!!summary.decisions?.length && (
              <p>Decisiones: {summary.decisions.join(" · ")}</p>
            )}
            {!!summary.artifacts?.length && (
              <p>Artefactos: {summary.artifacts.join(" · ")}</p>
            )}
            <button
              disabled={busy}
              onClick={() =>
                void action(
                  row.status === "running"
                    ? "memory.cancel_extraction"
                    : "memory.extract",
                  { conversation_id: row.conversation_id },
                )
              }
            >
              {row.status === "running"
                ? "Cancelar análisis"
                : "Analizar mensajes nuevos"}
            </button>
          </article>
        );
      })}
    </div>
  );
}
