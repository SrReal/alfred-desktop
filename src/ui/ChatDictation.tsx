import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import { PixelIcon } from "./PixelIcon";

type Session = {
  id: string;
  conversation_id: string;
  status: "starting" | "listening" | "stopping" | "review" | "error";
  text: string;
  error: string;
};
const errors: Record<string, string> = {
  PRIVACY_DENIED: "Desactiva Privacidad para iniciar el dictado.",
  DICTATION_SPANISH_REQUIRED:
    "Falta un reconocedor local de español. Instala el reconocimiento de voz de español en Configuración de Windows > Hora e idioma y vuelve a intentarlo.",
  DICTATION_MICROPHONE_UNAVAILABLE:
    "No se pudo abrir el micrófono predeterminado. Comprueba el dispositivo y el permiso de micrófono para aplicaciones de escritorio en Windows.",
  DICTATION_NO_SPEECH:
    "No se reconoció texto. Descarta este intento y vuelve a dictar.",
  DICTATION_ENGINE_UNAVAILABLE:
    "El reconocedor local de Windows no está disponible en este equipo.",
  DICTATION_RECOGNITION_FAILED:
    "El reconocimiento se interrumpió. Revisa el texto recuperado, si lo hay, o descarta este intento.",
  DICTATION_TEXT_LIMIT:
    "Se alcanzó el límite de texto. Revisa el fragmento recuperado; la última frase puede faltar.",
  DICTATION_TIMEOUT:
    "El dictado superó el tiempo permitido. Descarta este intento y vuelve a intentarlo.",
};
export function ChatDictation({
  active,
  conversationId,
  busy,
  onBusy,
  onDraft,
}: {
  active: boolean;
  conversationId: string;
  busy: boolean;
  onBusy: (busy: boolean) => void;
  onDraft: (text: string) => void;
}) {
  const [session, setSession] = useState<Session | null>(null);
  const [edited, setEdited] = useState("");
  const [error, setError] = useState("");
  const [working, setWorking] = useState(false);
  const current = useRef<Session | null>(null);
  const epoch = useRef(0);
  const actionLock = useRef(false);
  const reviewId = useRef("");
  useEffect(() => {
    onBusy(working || !!session);
  }, [working, session, onBusy]);
  useEffect(() => {
    const generation = ++epoch.current;
    let off: (() => void) | undefined;
    let revision = 0;
    current.current = null;
    setSession(null);
    setEdited("");
    setError("");
    setWorking(false);
    reviewId.current = "";
    const refresh = async () => {
      const version = ++revision;
      const value = await request<Session | null>("dictation.state");
      if (epoch.current !== generation || version !== revision) return;
      if (value && value.conversation_id !== conversationId) {
        await request("dictation.action", { id: value.id, action: "discard" });
        return;
      }
      current.current = value;
      setSession(value);
      if (!value) {
        setEdited("");
        reviewId.current = "";
      }
      if (value?.status === "review" && reviewId.current !== value.id) {
        reviewId.current = value.id;
        setEdited(value.text);
      }
    };
    if (active)
      void on("alfredpet:dictation", () => {
        void refresh().catch(() => {});
      }).then((fn) => {
        if (epoch.current !== generation) {
          fn();
          return;
        }
        off = fn;
        void refresh().catch((e) => {
          if (epoch.current === generation) setError(String(e));
        });
      });
    return () => {
      ++epoch.current;
      off?.();
      const value = current.current;
      current.current = null;
      if (value)
        void request("dictation.action", {
          id: value.id,
          action: "discard",
        }).catch(() => {});
      onBusy(false);
    };
  }, [active, conversationId, onBusy]);
  async function action(kind: "start" | "stop" | "discard" | "use") {
    if (actionLock.current) return;
    actionLock.current = true;
    setWorking(true);
    setError("");
    const generation = epoch.current;
    try {
      if (kind === "start") {
        const value = await request<Session>("dictation.start", {
          conversation_id: conversationId,
        });
        if (epoch.current !== generation) {
          await request("dictation.action", {
            id: value.id,
            action: "discard",
          });
          return;
        }
        // Events can already contain a newer state. Fetch the authoritative snapshot.
        const latest = await request<Session | null>("dictation.state");
        if (epoch.current !== generation) {
          await request("dictation.action", {
            id: value.id,
            action: "discard",
          });
          return;
        }
        current.current = latest;
        setSession(latest);
        if (latest?.status === "review") {
          reviewId.current = latest.id;
          setEdited(latest.text);
        }
      } else {
        const value = current.current;
        if (!value) return;
        const result = await request<{ text?: string }>("dictation.action", {
          id: value.id,
          action: kind,
          conversation_id: conversationId,
          text: edited,
        });
        if (epoch.current !== generation) return;
        if (kind === "use" && result?.text) onDraft(result.text);
        if (kind === "use" || kind === "discard") {
          current.current = null;
          setSession(null);
          setEdited("");
        }
      }
    } catch (e) {
      if (epoch.current === generation)
        setError(String(e).replace(/^Error: /, ""));
    } finally {
      actionLock.current = false;
      if (epoch.current === generation) setWorking(false);
    }
  }
  const listening =
    session && ["starting", "listening", "stopping"].includes(session.status);
  const problem = error || session?.error || "";
  return (
    <div className="chat-dictation">
      <button
        type="button"
        className="chat-tool-button"
        aria-label="Dictar"
        disabled={!active || busy || working || !!session}
        onClick={() => void action("start")}
        title="Dictar en español con el micrófono predeterminado. Revisar antes de enviar."
      >
        <PixelIcon name="microphone" />
      </button>
      {(session || problem) && (
        <section className="dictation-panel" aria-label="Dictado">
          <div role="status" className="dictation-status">
            {listening && <span className="dictation-dot" aria-hidden="true" />}
            {session?.status === "starting"
              ? "Preparando micrófono…"
              : session?.status === "listening"
                ? "Escuchando · máximo 2 minutos"
                : session?.status === "stopping"
                  ? "Deteniendo micrófono…"
                  : session?.status === "review"
                    ? "Revisa tu dictado"
                    : "Dictado"}
          </div>
          {listening && (
            <>
              <p className="dictation-transcript">
                {session.text || "Habla en español. El texto aparecerá aquí."}
              </p>
              <button
                disabled={working || session.status === "stopping"}
                onClick={() => void action("stop")}
              >
                Detener y revisar
              </button>
            </>
          )}
          {session?.status === "review" && (
            <>
              <textarea
                aria-label="Revisar dictado"
                value={edited}
                onChange={(e) => setEdited(e.target.value)}
                maxLength={12000}
              />
              <p>
                Corrige el texto y añádelo al mensaje. Después pulsa Enviar.
              </p>
              <button
                disabled={working || busy || !edited.trim()}
                onClick={() => void action("use")}
              >
                Añadir al mensaje
              </button>
            </>
          )}
          {session && (
            <button disabled={working} onClick={() => void action("discard")}>
              Descartar dictado
            </button>
          )}
          {problem && <p role="alert">{errors[problem] ?? problem}</p>}
          {!session && (
            <button onClick={() => setError("")}>Cerrar aviso</button>
          )}
          <small>
            Reconocimiento local. El audio no se guarda. Al salir del chat se
            descarta el dictado pendiente.
          </small>
        </section>
      )}
    </div>
  );
}
