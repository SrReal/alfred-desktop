import { useState } from "react";
import { request } from "../api";
import { PixelIcon } from "./PixelIcon";

export function ManualHelp() {
  const [busy, setBusy] = useState(false);
  const [opened, setOpened] = useState(false);
  const [error, setError] = useState("");
  async function open() {
    setBusy(true);
    setOpened(false);
    setError("");
    try {
      await request("manual.open");
      setOpened(true);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }
  return (
    <article className="card">
      <h2>Ayuda</h2>
      <p>
        Consulta el manual completo: configuración, chat, tareas, memoria,
        módulos, skills y plugins. Está incluido y disponible sin conexión.
      </p>
      <button disabled={busy} onClick={() => void open()}>
        <PixelIcon name="file" />
        {busy ? "Abriendo manual…" : "Manual de uso (PDF)"}
      </button>
      {opened && (
        <p role="status">Manual enviado al visor de PDF de Windows.</p>
      )}
      {error && <p role="alert">{error}</p>}
    </article>
  );
}
