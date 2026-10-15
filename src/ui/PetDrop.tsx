import { useCallback, useEffect, useRef, useState } from "react";
import { on, request } from "../api";
type Item = {
  id: string;
  path: string;
  name: string;
  created_at: string;
  error: string | null;
};
type Pending = { items: Item[]; last_error: string | null };
type Result = {
  conversation_id: string;
  attached: { id: string }[];
  failed: { id: string; name: string; error: string }[];
};
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
export function PetDrop({
  conversations,
  busy: chatBusy,
  onAttached,
}: {
  conversations: { id: string; title: string }[];
  busy: boolean;
  onAttached: (conversation: string, prepareAnalysis: boolean) => void;
}) {
  const [pending, setPending] = useState<Pending>({
    items: [],
    last_error: null,
  });
  const [selected, setSelected] = useState<string[]>([]);
  const [conversation, setConversation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const generation = useRef(0);
  const live = useRef(true);
  const working = useRef(false);
  const refresh = useCallback(async () => {
    const current = ++generation.current;
    try {
      const data = await request<Pending>("pet_drop.list");
      if (
        !data ||
        !Array.isArray(data.items) ||
        data.items.length > 20 ||
        (data.last_error !== null && typeof data.last_error !== "string") ||
        data.items.some(
          (item) =>
            !item ||
            typeof item.id !== "string" ||
            typeof item.name !== "string" ||
            typeof item.path !== "string",
        )
      )
        throw new Error("DROP_QUEUE_RESPONSE");
      if (live.current && current === generation.current) {
        setPending(data);
        setSelected((ids) =>
          ids.filter((id) => data.items.some((item) => item.id === id)),
        );
      }
    } catch (e) {
      if (live.current && current === generation.current)
        setError(errorText(e));
    }
  }, []);
  useEffect(() => {
    live.current = true;
    void refresh();
    let stop: (() => void) | undefined;
    void on("alfredpet:drops", () => void refresh()).then((off) => {
      if (live.current) stop = off;
      else off();
    });
    return () => {
      live.current = false;
      ++generation.current;
      stop?.();
    };
  }, [refresh]);
  async function action(prepareAnalysis: boolean, dismiss = false) {
    if (working.current || chatBusy || !selected.length) return;
    working.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      if (dismiss) {
        await request("pet_drop.dismiss", { ids: selected });
        if (live.current)
          setNotice(
            "Selección descartada. Los archivos originales se conservan.",
          );
      } else {
        const target =
          conversation ||
          (await request<{ id: string }>("conversations.create")).id;
        if (!live.current) return;
        setConversation(target);
        const result = await request<Result>("pet_drop.attach", {
          ids: selected,
          conversation_id: target,
        });
        if (live.current) {
          setConversation(target);
          if (result.attached.length) {
            onAttached(result.conversation_id, prepareAnalysis);
            setNotice(
              `${result.attached.length} archivo(s) adjuntado(s). Los originales se conservan.${prepareAnalysis ? " Revisa el borrador y pulsa Enviar para analizar." : ""}`,
            );
          }
          if (result.failed.length)
            setError(
              result.failed
                .map((item) => `${item.name}: ${item.error}`)
                .join(" · "),
            );
        }
      }
      await refresh();
    } catch (e) {
      if (live.current) setError(errorText(e));
    } finally {
      working.current = false;
      if (live.current) setBusy(false);
    }
  }
  const disabled = busy || chatBusy;
  if (!pending.items.length && !pending.last_error && !error && !notice)
    return null;
  return (
    <section
      className="card pet-drop-review"
      aria-label="Archivos soltados sobre AlfredPet"
    >
      <h2>Archivos soltados sobre AlfredPet</h2>
      {(error || pending.last_error) && (
        <p role="alert">{error || pending.last_error}</p>
      )}
      {notice && <p role="status">{notice}</p>}
      {!!pending.items.length && (
        <>
          <p>
            Selecciona hasta cinco archivos y la conversación de destino. La
            copia se crea al adjuntar.
          </p>
          {pending.items.map((item) => (
            <label key={item.id} className="drop-file">
              <input
                type="checkbox"
                checked={selected.includes(item.id)}
                disabled={
                  disabled ||
                  (!selected.includes(item.id) && selected.length >= 5)
                }
                onChange={(e) =>
                  setSelected((ids) =>
                    e.target.checked
                      ? [...ids, item.id]
                      : ids.filter((id) => id !== item.id),
                  )
                }
              />
              <span>
                {item.name}
                <small>{item.path}</small>
                {item.error && <small role="alert">{item.error}</small>}
              </span>
            </label>
          ))}
          <label>
            Conversación para los archivos
            <select
              value={conversation}
              onChange={(e) => setConversation(e.target.value)}
              disabled={disabled}
            >
              <option value="">Nueva conversación</option>
              {conversations.map((c) => (
                <option value={c.id} key={c.id}>
                  {c.title}
                </option>
              ))}
            </select>
          </label>
          <div className="row">
            <button
              disabled={disabled || !selected.length}
              onClick={() => void action(false)}
            >
              Adjuntar selección
            </button>
            <button
              disabled={disabled || !selected.length}
              onClick={() => void action(true)}
            >
              Adjuntar y preparar análisis
            </button>
            <button
              disabled={disabled || !selected.length}
              onClick={() => void action(false, true)}
            >
              Descartar selección
            </button>
          </div>
        </>
      )}
    </section>
  );
}
