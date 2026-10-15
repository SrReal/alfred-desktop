import { useEffect, useState } from "react";
import { request } from "../api";
import { PathPicker } from "./PathPicker";

type Preview = {
  counts: Record<string, number>;
  sha256?: string;
  created_at?: string;
  files?: number;
  bytes?: number;
};
type Receipt = {
  status: string;
  operation: string;
  at: string;
  error?: string;
  result?: { path?: string; recovery_archive?: string };
};
const labels: Record<string, string> = {
  conversations: "Conversaciones",
  messages: "Mensajes",
  attachments: "Adjuntos",
  custom_tools: "Plugins",
  tasks: "Tareas",
  memories: "Recuerdos",
};

export function ProfileBackup() {
  const [mode, setMode] = useState<"backup" | "restore">("backup");
  const [path, setPath] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [queued, setQueued] = useState(false);
  useEffect(() => {
    let active = true;
    void request<Receipt | null>("profile.status")
      .then((v) => {
        if (active) setReceipt(v);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);
  function reset(nextMode: "backup" | "restore", nextPath = "") {
    setMode(nextMode);
    setPath(nextPath);
    setPreview(null);
    setConfirmed(false);
    setError("");
  }
  async function review() {
    setBusy(true);
    setError("");
    setPreview(null);
    setConfirmed(false);
    try {
      const value = await request<Preview>(`profile.preview_${mode}`, { path });
      setPreview(value);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function run() {
    if (!preview || !confirmed) return;
    setBusy(true);
    setError("");
    try {
      await request("profile.schedule", {
        mode,
        path,
        sha256: preview.sha256 ?? "",
        confirmed: true,
      });
      setQueued(true);
      await request("profile.restart");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section
      className="profile-backup"
      aria-label="Copia completa y restauración"
    >
      <h3>Copia completa y restauración</h3>
      <p>
        Guarda conversaciones, adjuntos, recuerdos, tareas, preferencias, skills
        y archivos de plugins. Usa una unidad con espacio para la copia y el
        perfil temporal.
      </p>
      {receipt && (
        <div className="profile-backup-receipt" role="status">
          <strong>
            {receipt.status === "completed"
              ? "Operación terminada"
              : "La operación no se completó"}
          </strong>
          <p>
            {new Date(receipt.at).toLocaleString()} ·{" "}
            {receipt.operation === "backup" ? "Copia" : "Restauración"}
          </p>
          {receipt.error && <p>{receipt.error}</p>}
          {receipt.result?.path && (
            <p className="profile-backup-path">{receipt.result.path}</p>
          )}
          {receipt.result?.recovery_archive && (
            <>
              <p>
                El perfil anterior está guardado. Puedes revisarlo y restaurarlo
                con este mismo asistente.
              </p>
              <p className="profile-backup-path">
                {receipt.result.recovery_archive}
              </p>
              <button
                disabled={busy || queued}
                onClick={() =>
                  reset("restore", receipt.result?.recovery_archive)
                }
              >
                Revisar copia anterior
              </button>
            </>
          )}
        </div>
      )}
      {!queued ? (
        <>
          <div className="row">
            <button
              disabled={busy}
              aria-pressed={mode === "backup"}
              onClick={() => reset("backup")}
            >
              Crear copia
            </button>
            <button
              disabled={busy}
              aria-pressed={mode === "restore"}
              onClick={() => reset("restore")}
            >
              Restaurar copia
            </button>
          </div>
          <PathPicker
            label={
              mode === "backup" ? "Guardar copia como…" : "Seleccionar copia…"
            }
            mode={mode === "backup" ? "save" : "file"}
            value={path}
            onChange={(value) => reset(mode, value)}
            disabled={busy}
            extensions={["alfredbackup"]}
            defaultName={
              mode === "backup"
                ? `AlfredPet-${new Date().toISOString().slice(0, 10)}.alfredbackup`
                : ""
            }
          />
          <button disabled={!path || busy} onClick={() => void review()}>
            {busy ? "Comprobando…" : "Revisar contenido"}
          </button>
          {preview && (
            <div className="profile-backup-review">
              <h4>
                {mode === "backup"
                  ? "Contenido del perfil actual"
                  : "Copia comprobada"}
              </h4>
              {preview.created_at && (
                <p>
                  Creada: {new Date(preview.created_at).toLocaleString()} ·{" "}
                  {preview.files} archivos ·{" "}
                  {((preview.bytes ?? 0) / 1048576).toFixed(1)} MiB
                </p>
              )}
              <dl className="profile-backup-counts">
                {Object.entries(preview.counts).map(([key, count]) => (
                  <div key={key}>
                    <dt>{labels[key] ?? key}</dt>
                    <dd>{count}</dd>
                  </div>
                ))}
              </dl>
              <p>
                La copia contiene datos privados y no está cifrada. No incluye
                credenciales de Windows, sesión de Claude, cachés, modelos
                descargados ni carpetas de proyectos externos. Límites: 2 GiB en
                total, 512 MiB por archivo y 10 000 archivos.
              </p>
              {mode === "restore" && (
                <p>
                  Se sustituye todo el perfil, sin mezclar conversaciones.
                  Primero se guarda una copia del perfil actual. Se activa
                  Privacidad y se desactivan módulos, plugins, macros y
                  vigilancias; tendrás que revisar permisos, carpetas y
                  conexiones. Los plugins vuelven a borrador. El historial
                  visible se conserva; las sesiones de Claude no se transfieren.
                </p>
              )}
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={confirmed}
                  disabled={busy}
                  onChange={(e) => setConfirmed(e.target.checked)}
                />
                {mode === "backup"
                  ? "Entiendo que contiene datos privados y que Alfred se reiniciará."
                  : "Confirmo que quiero sustituir el perfil y revisar sus permisos al terminar."}
              </label>
              <div className="row">
                <button
                  disabled={busy || !confirmed}
                  onClick={() => void run()}
                >
                  Reiniciar y {mode === "backup" ? "crear copia" : "restaurar"}
                </button>
                <button
                  disabled={busy}
                  onClick={() => {
                    setPreview(null);
                    setConfirmed(false);
                  }}
                >
                  Cancelar
                </button>
              </div>
              <p>
                Espera a que Alfred vuelva a abrirse. El resultado aparecerá
                aquí. Si falla, se conserva o recupera el perfil anterior.
              </p>
            </div>
          )}
        </>
      ) : (
        <p role="status">
          Operación preparada. Alfred se reiniciará para terminarla. Si sigue
          abierto, sal desde el icono de la bandeja y vuelve a abrirlo.
        </p>
      )}
      {error && <p role="alert">{error}</p>}
    </section>
  );
}
