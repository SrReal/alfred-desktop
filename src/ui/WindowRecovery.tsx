import { useCallback, useEffect, useState } from "react";
import { on, request } from "../api";

type Journal = {
  id: string;
  description: string;
  state: string;
  error?: string | null;
};
export function WindowRecovery() {
  const [rows, setRows] = useState<Journal[]>([]);
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const reload = useCallback(async () => {
    try {
      const data = await request<Journal[]>("tools.execute", {
        tool: "window.undo_history",
        input: {},
      });
      if (!Array.isArray(data))
        throw new Error("Respuesta de recuperación inválida.");
      setRows(data);
      setLoadError("");
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  useEffect(() => {
    void reload();
    let live = true;
    let stop: (() => void) | undefined;
    void on("alfredpet:activity", () => void reload()).then((listener) => {
      if (live) stop = listener;
      else listener();
    });
    return () => {
      live = false;
      stop?.();
    };
  }, [reload]);
  async function undo(row: Journal) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await request<{ reconciled?: boolean }>("tools.execute", {
        tool: "window.undo",
        input: {
          id: row.id,
          ...(row.state === "blocked" ? { revalidate: true } : {}),
        },
      });
      await reload();
      setNotice(
        result.reconciled
          ? "Estado de ventanas verificado."
          : "Posición y estado anteriores restaurados.",
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <h1>Recuperación de ventanas</h1>
      <p>
        Deshacer requiere confirmación. Si una ventana cambió, fue sustituida o
        cambió la configuración de pantallas, se conserva su estado actual.
      </p>
      {error && <p role="alert">{error}</p>}
      {loadError && <p role="alert">{loadError}</p>}
      {notice && <p role="status">{notice}</p>}
      <button disabled={busy} onClick={() => void reload()}>
        Actualizar recuperaciones
      </button>
      {rows.map((row) => (
        <article className="card" key={row.id}>
          <strong>{row.description}</strong>
          <p>
            {row.state === "available"
              ? "Disponible"
              : row.state === "blocked"
                ? "Requiere revisión"
                : row.state === "undone"
                  ? "Deshecho"
                  : "Operación incompleta"}
          </p>
          {row.error && <p>{row.error}</p>}
          {["available", "blocked"].includes(row.state) && (
            <button disabled={busy} onClick={() => void undo(row)}>
              {row.state === "blocked"
                ? "Revalidar y solicitar deshacer"
                : "Solicitar deshacer"}
            </button>
          )}
        </article>
      ))}
      {!rows.length && !error && !loadError && (
        <p>No hay recuperaciones de ventanas.</p>
      )}
    </>
  );
}
