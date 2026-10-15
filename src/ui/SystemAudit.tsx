import { useState } from "react";
import { request } from "../api";
import { AuditHistory, type AuditReport } from "./AuditHistory";
const actions = [
  ["defender_update", "Actualizar firmas de Defender"],
  ["defender_quick_scan", "Análisis rápido de Defender"],
  ["defender_full_scan", "Análisis completo de Defender"],
  ["sfc_verify", "Comprobar archivos de Windows"],
  ["sfc_repair", "Reparar archivos de Windows (SFC)"],
  ["dism_check", "Comprobar imagen de Windows"],
  ["dism_repair", "Reparar imagen de Windows (DISM)"],
];
export function SystemAudit() {
  const [report, setReport] = useState<AuditReport>();
  const [revision, setRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [action, setAction] = useState(actions[0][0]);
  const [output, setOutput] = useState("");
  async function audit(elevated = false) {
    setBusy(true);
    setError("");
    try {
      setReport(
        await request<AuditReport>("tools.execute", {
          tool: "windows.audit",
          input: { elevated },
        }),
      );
      setRevision((v) => v + 1);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function execute() {
    setBusy(true);
    setError("");
    setOutput("");
    try {
      const value = await request<{
        result: { stdout: string; stderr: string };
        note: string;
      }>("tools.execute", { tool: "windows.admin_action", input: { action } });
      setOutput(value.result.stdout + value.result.stderr + "\n" + value.note);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section>
      <h1>Equipo y seguridad</h1>
      <p>
        Consulta Windows, servicios, inicio, discos y Microsoft Defender. Un
        error o un programa desconocido necesita revisión; no demuestra que
        exista un virus. Cada consulta completada se guarda localmente para
        compararla.
      </p>
      <div className="row">
        <button disabled={busy} onClick={() => void audit()}>
          Auditar equipo
        </button>
        <button disabled={busy} onClick={() => void audit(true)}>
          Auditar con autorización UAC
        </button>
      </div>
      {busy && (
        <p role="status">
          Trabajando… Atiende los permisos de Alfred y el aviso de Windows. Un
          análisis completo puede tardar bastante.
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      <AuditHistory revision={revision} disabled={busy} onReport={setReport} />
      {report && (
        <>
          <p>Última consulta: {new Date(report.checkedAt).toLocaleString()}</p>
          {report.origin && (
            <p>
              Origen: {report.origin.computer} ·{" "}
              {report.elevated ? "Con autorización UAC" : "Sin elevar"}
            </p>
          )}
          <p>{report.scope}</p>
          {report.sections.map((s, i) => (
            <article className="card" key={i}>
              <h2>{s.name}</h2>
              <p>
                {s.status === "observed"
                  ? "Consulta completada; revisa los datos"
                  : "No disponible: " + s.error}
              </p>
              <details>
                <summary>Ver resultados</summary>
                <pre>{JSON.stringify(s.data ?? {}, null, 2)}</pre>
              </details>
            </article>
          ))}
        </>
      )}
      <article className="card">
        <h2>Acciones administrativas</h2>
        <p>
          Requieren el módulo Windows activado, revisión de la acción y
          autorización UAC. Defender puede aplicar su política habitual de
          cuarentena. SFC y DISM pueden modificar archivos del sistema; no
          reinician el equipo automáticamente.
        </p>
        <label>
          Operación
          <select
            aria-label="Operación administrativa"
            value={action}
            disabled={busy}
            onChange={(e) => setAction(e.target.value)}
          >
            {actions.map(([id, label]) => (
              <option key={id} value={id}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <button disabled={busy} onClick={() => void execute()}>
          Revisar y ejecutar
        </button>
        {output && <pre role="status">{output}</pre>}
        <p>
          Si se interrumpe la espera, un análisis de Defender ya iniciado puede
          continuar. Consulta Seguridad de Windows antes de repetirlo.
        </p>
      </article>
    </section>
  );
}
