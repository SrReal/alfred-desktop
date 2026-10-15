import { useEffect, useRef, useState } from "react";
import { request } from "../api";

type Summary = {
  id: string;
  checkedAt: string;
  origin: { id: string; computer: string };
  elevated: boolean;
  unavailable: number;
};
export type AuditReport = Summary & {
  scope: string;
  sections: { name: string; status: string; data?: unknown; error?: string }[];
};
type Comparison = {
  before: Summary;
  after: Summary;
  changedSections: number;
  incompleteSections: number;
  differentElevation: boolean;
  sections: {
    name: string;
    source: string;
    status: string;
    beforeStatus: string;
    afterStatus: string;
    note: string;
    changes: {
      label: string;
      explanation: string;
      kind: string;
      before: unknown;
      after: unknown;
    }[];
  }[];
};
const stamp = (s: Summary) =>
  `${new Date(s.checkedAt).toLocaleString()} · ${s.origin.computer} (${s.origin.id.slice(0, 8)}) · ${s.elevated ? "UAC" : "sin elevar"}`;
const statusName: Record<string, string> = {
  changed: "Cambios observados",
  unchanged: "Sin cambios en los datos observados",
  unavailable: "Comparación incompleta",
  unsupported: "Datos no comparables",
  observed: "disponible",
  missing: "sin sección",
};
const valueText = (v: unknown) =>
  v == null ? "No observado" : JSON.stringify(v, null, 2);

export function AuditHistory({
  revision,
  disabled,
  onReport,
}: {
  revision: number;
  disabled: boolean;
  onReport: (report?: AuditReport) => void;
}) {
  const [rows, setRows] = useState<Summary[]>([]);
  const [before, setBefore] = useState("");
  const [after, setAfter] = useState("");
  const [comparison, setComparison] = useState<Comparison>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const epoch = useRef(0);
  useEffect(() => {
    const current = ++epoch.current;
    setBusy(true);
    setError("");
    setComparison(undefined);
    setDeleting(false);
    void request<Summary[]>("system_audit.list")
      .then((values) => {
        if (epoch.current !== current) return;
        setRows(values);
        setAfter(values[0]?.id ?? "");
        setBefore(
          values.find(
            (v) =>
              v.id !== values[0]?.id &&
              v.origin.id === values[0]?.origin.id &&
              new Date(v.checkedAt) < new Date(values[0].checkedAt),
          )?.id ?? "",
        );
      })
      .catch((e) => {
        if (epoch.current === current) {
          setRows([]);
          setBefore("");
          setAfter("");
          setError(String(e));
        }
      })
      .finally(() => {
        if (epoch.current === current) setBusy(false);
      });
    return () => {
      epoch.current++;
    };
  }, [revision, refresh]);
  async function action(op: "compare" | "get" | "delete") {
    const current = epoch.current;
    setBusy(true);
    setError("");
    setComparison(undefined);
    try {
      const result = await request<Comparison & AuditReport>(
        `system_audit.${op}`,
        op === "compare"
          ? { beforeId: before, afterId: after }
          : { id: after, ...(op === "delete" ? { confirmed: true } : {}) },
      );
      if (epoch.current !== current) return;
      if (op === "compare") setComparison(result);
      else if (op === "get") onReport(result);
      else {
        onReport(undefined);
        setRefresh((v) => v + 1);
      }
    } catch (e) {
      if (epoch.current === current) setError(String(e));
    } finally {
      if (epoch.current === current) setBusy(false);
    }
  }
  function select(which: "before" | "after", id: string) {
    if (which === "before") setBefore(id);
    else setAfter(id);
    setComparison(undefined);
    setDeleting(false);
    setError("");
  }
  const locked = busy || disabled;
  return (
    <article className="card audit-history">
      <h2>Auditorías guardadas</h2>
      <p>
        Se conservan las últimas 20 consultas en este perfil. Comparar usa los
        datos guardados; no ejecuta una auditoría ni una reparación.
      </p>
      <button disabled={locked} onClick={() => setRefresh((v) => v + 1)}>
        Actualizar historial
      </button>
      {busy && <p role="status">Consultando historial…</p>}
      {error && <p role="alert">{error}</p>}
      {!busy && rows.length === 0 && !error && (
        <p>No hay auditorías guardadas. Ejecuta una consulta para empezar.</p>
      )}
      {rows.length > 0 && (
        <>
          <div className="audit-selectors">
            <label>
              Auditoría anterior
              <select
                aria-label="Auditoría anterior"
                disabled={locked}
                value={before}
                onChange={(e) => select("before", e.target.value)}
              >
                <option value="">Seleccionar anterior</option>
                {rows.map((s) => (
                  <option key={s.id} value={s.id}>
                    {stamp(s)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Auditoría reciente
              <select
                aria-label="Auditoría reciente"
                disabled={locked}
                value={after}
                onChange={(e) => select("after", e.target.value)}
              >
                <option value="">Seleccionar reciente</option>
                {rows.map((s) => (
                  <option key={s.id} value={s.id}>
                    {stamp(s)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className="row">
            <button
              disabled={locked || !before || !after || before === after}
              onClick={() => void action("compare")}
            >
              Comparar auditorías
            </button>
            <button
              disabled={locked || !after}
              onClick={() => void action("get")}
            >
              Ver auditoría reciente
            </button>
            <button
              disabled={locked || !after}
              onClick={() => setDeleting(true)}
            >
              Borrar auditoría reciente
            </button>
          </div>
          {deleting && (
            <div role="group" aria-label="Confirmar borrado de auditoría">
              <p>
                Se borrará del historial:{" "}
                {stamp(rows.find((r) => r.id === after)!)}. Las copias de
                seguridad anteriores se conservan.
              </p>
              <div className="row">
                <button disabled={locked} onClick={() => void action("delete")}>
                  Confirmar borrado
                </button>
                <button disabled={locked} onClick={() => setDeleting(false)}>
                  Cancelar borrado
                </button>
              </div>
            </div>
          )}
          {rows.length < 2 && (
            <p>
              Necesitas dos auditorías del mismo origen y con fechas distintas.
            </p>
          )}
        </>
      )}
      {comparison && (
        <section aria-label="Resultado de comparación">
          <h3>Comparación de auditorías</h3>
          <p>
            Anterior: {stamp(comparison.before)}
            <br />
            Reciente: {stamp(comparison.after)}
          </p>
          <p>
            {comparison.changedSections} secciones con cambios ·{" "}
            {comparison.incompleteSections} incompletas o no comparables.
          </p>
          {comparison.differentElevation && (
            <p>
              Las consultas se ejecutaron con permisos distintos. Esto puede
              cambiar los datos disponibles.
            </p>
          )}
          <p>
            Los resultados describen observaciones, no garantizan que el equipo
            esté libre de malware.
          </p>
          {comparison.sections.map((s) => (
            <section className="audit-section" key={s.name}>
              <h3>{s.name}</h3>
              <p>
                <strong>{statusName[s.status] ?? s.status}</strong> · Fuente:{" "}
                {s.source}
              </p>
              <p>{s.note}</p>
              {s.status === "unavailable" && (
                <p>
                  Anterior: {statusName[s.beforeStatus] ?? "no disponible"}.
                  Reciente: {statusName[s.afterStatus] ?? "no disponible"}. No
                  se deducen altas, bajas ni mejoras de datos ausentes.
                </p>
              )}
              {s.status === "unsupported" && (
                <p>
                  La estructura o las identidades de los datos son ambiguas.
                  Revisa los informes originales.
                </p>
              )}
              {s.changes.map((change, i) => (
                <details key={i}>
                  <summary>
                    {change.label} · {change.explanation}
                  </summary>
                  <div className="audit-values">
                    <div>
                      <h4>Anterior</h4>
                      <pre>{valueText(change.before)}</pre>
                    </div>
                    <div>
                      <h4>Reciente</h4>
                      <pre>{valueText(change.after)}</pre>
                    </div>
                  </div>
                </details>
              ))}
            </section>
          ))}
        </section>
      )}
    </article>
  );
}
