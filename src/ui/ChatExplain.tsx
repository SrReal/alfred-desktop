import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import { ChatAssetImage } from "./ManagedAssets";
import { PixelIcon } from "./PixelIcon";

type Preview = {
  id: string;
  text?: string;
  asset_id?: string;
  notice?: string;
};
const notices: Record<string, string> = {
  EXPLAIN_NO_SELECTION:
    "No se pudo leer una selección de texto. Selecciona una región o copia y pega el texto en el mensaje.",
  EXPLAIN_SELECTION_TIMEOUT:
    "La aplicación no respondió a tiempo. Puedes seleccionar una región.",
  EXPLAIN_SELECTION_LIMIT:
    "La selección es demasiado extensa (máximo 12 KB). Selecciona un fragmento más corto.",
  EXPLAIN_SENSITIVE_SELECTION:
    "La selección parece contener credenciales. Se ha descartado; selecciona solo el fragmento que necesitas explicar.",
  EXPLAIN_GUI_DISABLED:
    "Activa el módulo GUI para leer texto seleccionado, o utiliza una región con el módulo Visión.",
  EXPLAIN_VISION_DISABLED:
    "Activa el módulo Visión para seleccionar una región.",
  EXPLAIN_PREVIEW_PENDING:
    "Ya hay una vista previa en el chat. Úsala o descártala antes de añadir otra.",
  PRIVACY_DENIED:
    "Desactiva el modo Privacidad si quieres compartir una selección.",
};
export function explainError(error: unknown) {
  const key = error instanceof Error ? error.message : String(error);
  return notices[key] ?? key;
}
export function ChatExplain({
  active = true,
  busy,
  conversation,
  onBusy,
  onDraft,
}: {
  active?: boolean;
  busy: boolean;
  conversation: () => Promise<string>;
  onBusy: (busy: boolean) => void;
  onDraft: (text: string) => void;
}) {
  const [preview, setPreview] = useState<Preview | null>(null);
  const [open, setOpen] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  const lock = useRef(false);
  useEffect(() => {
    if (!active) {
      setPreview(null);
      setError("");
      return;
    }
    let live = true;
    let epoch = 0;
    let off: (() => void) | undefined;
    const refresh = async () => {
      const n = ++epoch;
      try {
        const data = await request<Preview | null>("explain.pending");
        if (live && epoch === n) {
          setPreview(data?.id ? data : null);
          setError("");
          if (data?.id) setOpen(true);
        }
      } catch (e) {
        if (live && epoch === n) setError(explainError(e));
      }
    };
    void on("alfredpet:explain", () => void refresh()).then((stop) => {
      if (!live) return stop();
      off = stop;
      void refresh();
    });
    return () => {
      live = false;
      off?.();
    };
  }, [active]);
  async function action(kind: "prepare" | "discard" | "region") {
    if (lock.current || busy) return;
    lock.current = true;
    setWorking(true);
    onBusy(true);
    setError("");
    try {
      if (kind === "prepare" && preview) {
        const id = await conversation();
        const result = await request<{ text: string }>("explain.prepare", {
          id: preview.id,
          conversation_id: id,
        });
        onDraft(result.text);
        setOpen(false);
      } else {
        if (preview)
          await request("explain.prepare", { id: preview.id, dismiss: true });
        if (kind === "region") await request("explain.region");
        else setOpen(false);
      }
      setPreview(null);
    } catch (e) {
      setError(explainError(e));
    } finally {
      lock.current = false;
      setWorking(false);
      onBusy(false);
    }
  }
  return (
    <div className="chat-explain">
      <button type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        <PixelIcon name="capture" /> Explícame esto
        {preview ? " · vista previa" : ""}
      </button>
      {open && (
        <section className="explain-preview" aria-label="Explicar selección">
          <p>
            Texto: <kbd>Ctrl+Alt+U</kbd> en la aplicación de origen. Región:{" "}
            <kbd>Ctrl+Alt+Mayús+U</kbd> sobre el monitor deseado.
          </p>
          {preview?.notice && (
            <p role="status">{notices[preview.notice] ?? preview.notice}</p>
          )}
          {preview?.text && <pre className="explain-text">{preview.text}</pre>}
          {preview?.asset_id && (
            <ChatAssetImage
              id={preview.asset_id}
              alt="Región seleccionada para explicar"
            />
          )}
          {(preview?.text || preview?.asset_id) && (
            <p>
              Revisa el contenido antes de compartirlo. Se añadirá al borrador
              de la conversación seleccionada; pulsa Enviar cuando esté listo.
            </p>
          )}
          <div className="row">
            {(preview?.text || preview?.asset_id) && (
              <button
                disabled={busy || working}
                onClick={() => void action("prepare")}
              >
                Usar en este chat
              </button>
            )}
            <button
              disabled={busy || working}
              onClick={() => void action("region")}
            >
              Seleccionar región
            </button>
            {preview && (
              <button
                disabled={busy || working}
                onClick={() => void action("discard")}
              >
                Descartar selección
              </button>
            )}
          </div>
          {error && <p role="alert">{error}</p>}
        </section>
      )}
    </div>
  );
}
