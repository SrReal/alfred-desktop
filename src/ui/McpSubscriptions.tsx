import { useEffect, useRef, useState } from "react";
import { request } from "../api";
import type { Json } from "../types";

type Stream = Record<string, Json> & {
  stream_id: string;
  connection: string;
  status: string;
};
export function McpSubscriptions({
  connection,
  profile,
  onChanged,
}: {
  connection: string;
  profile: Json;
  onChanged: () => void;
}) {
  const [tools, setTools] = useState(false);
  const [resources, setResources] = useState(false);
  const [prompts, setPrompts] = useState(false);
  const [uris, setUris] = useState<string[]>([]);
  const [streams, setStreams] = useState<Stream[]>([]);
  const [events, setEvents] = useState<Json[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const epoch = useRef(0);
  useEffect(() => {
    epoch.current++;
    setStreams([]);
    setEvents([]);
    setError("");
    setTools(false);
    setResources(false);
    setPrompts(false);
    setUris([]);
    setBusy(false);
    return () => {
      epoch.current++;
    };
  }, [connection, profile]);
  const allowed = (profile as Record<string, Json>).permissions as
    Record<string, Json> | undefined;
  const resourceUris = Array.isArray(allowed?.resources)
    ? allowed.resources.filter((v): v is string => typeof v === "string")
    : [];
  async function run(action: string, input: Record<string, Json> = {}) {
    if (pending.current) return;
    pending.current = true;
    const ticket = epoch.current;
    setBusy(true);
    setError("");
    setEvents([]);
    try {
      const result = await request<Record<string, Json>>("tools.execute", {
        tool: `mcp.listen${action}`,
        input: { connection, ...input },
      });
      if (ticket !== epoch.current) return;
      const records = action === "_status" ? result.streams : [result];
      if (
        !Array.isArray(records) ||
        records.some(
          (v) =>
            !v ||
            typeof v !== "object" ||
            Array.isArray(v) ||
            v.connection !== connection ||
            typeof v.stream_id !== "string" ||
            typeof v.status !== "string",
        )
      )
        throw new Error("El estado de la suscripción MCP no es válido.");
      if (action === "_status") setStreams(records as Stream[]);
      else
        setStreams((old) => [
          ...old.filter((v) => v.stream_id !== result.stream_id),
          result as Stream,
        ]);
      if (action === "_poll") {
        if (!Array.isArray(result.events))
          throw new Error("Los avisos MCP no son válidos.");
        setEvents(result.events);
        if (
          result.events.length ||
          (typeof result.lost === "number" && result.lost > 0)
        )
          onChanged();
      }
    } catch (e) {
      if (ticket === epoch.current) {
        setError(e instanceof Error ? e.message : String(e));
        setEvents([]);
        if (action === "_poll") onChanged();
      }
    } finally {
      pending.current = false;
      if (ticket === epoch.current) setBusy(false);
    }
  }
  return (
    <section className="card" aria-label="Suscripciones MCP">
      <h3>Avisos del servidor MCP</h3>
      <p>
        Elige qué avisos recibir. Iniciar requiere aprobación individual. La
        conexión dura hasta 15 minutos y se cierra al revocar permisos. No se
        reconecta automáticamente ni garantiza recuperar avisos anteriores.
      </p>
      {(
        [
          ["Cambios de herramientas MCP", tools, setTools],
          ["Cambios de recursos MCP", resources, setResources],
          ["Cambios de prompts MCP", prompts, setPrompts],
        ] as const
      ).map(([label, checked, setter]) => (
        <label key={label}>
          <input
            type="checkbox"
            disabled={busy}
            checked={checked}
            onChange={(e) => setter(e.target.checked)}
          />
          {label}
        </label>
      ))}
      {resourceUris.map((uri) => (
        <label key={uri}>
          <input
            type="checkbox"
            aria-label={`Avisos de ${uri}`}
            disabled={busy}
            checked={uris.includes(uri)}
            onChange={(e) =>
              setUris((old) =>
                e.target.checked ? [...old, uri] : old.filter((v) => v !== uri),
              )
            }
          />
          {uri}
        </label>
      ))}
      <button
        disabled={
          busy ||
          (!tools && !resources && !prompts && !uris.length) ||
          uris.length > 32
        }
        onClick={() =>
          void run("", {
            notifications: {
              ...(tools ? { toolsListChanged: true } : {}),
              ...(resources ? { resourcesListChanged: true } : {}),
              ...(prompts ? { promptsListChanged: true } : {}),
              ...(uris.length ? { resourceSubscriptions: uris } : {}),
            },
          })
        }
      >
        Iniciar suscripción MCP
      </button>
      <button disabled={busy} onClick={() => void run("_status")}>
        Consultar suscripciones MCP
      </button>
      {streams.map((stream) => (
        <article key={stream.stream_id}>
          <p>
            Suscripción {stream.stream_id} · {stream.status}
          </p>
          <p>
            Filtro aceptado por el servidor:{" "}
            {JSON.stringify(stream.honored ?? null)}
          </p>
          <p>
            Avisos recibidos: {String(stream.received ?? 0)} · descartados:{" "}
            {String(stream.lost ?? 0)} · pendientes:{" "}
            {String(stream.queued ?? 0)}
          </p>
          {stream.reason && <p>{String(stream.reason)}</p>}
          <button
            disabled={busy || stream.status !== "active"}
            onClick={() =>
              void run("_poll", { stream_id: stream.stream_id, max_events: 32 })
            }
          >
            Leer avisos MCP
          </button>
          <button
            disabled={busy || stream.status === "closed"}
            onClick={() => void run("_stop", { stream_id: stream.stream_id })}
          >
            Detener suscripción MCP
          </button>
        </article>
      ))}
      {error && <p role="alert">{error}</p>}
      {events.length > 0 && (
        <pre aria-label="Avisos MCP recibidos">
          {JSON.stringify(events, null, 2)}
        </pre>
      )}
    </section>
  );
}
