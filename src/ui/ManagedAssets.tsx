import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { on, request } from "../api";
import type { Attachment, Json } from "../types";

type Asset = {
  id: string;
  label: string;
  state: string;
  class: string;
  width: number;
  height: number;
  size: number;
  expires_at: string | null;
  reference_count: number;
  thumbnail_data_url?: string;
  error?: string;
};
const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));
const tools = <T,>(tool: string, input: Json = {}) =>
  request<T>("tools.execute", { tool, input });

function useAssetEvents(refresh: () => void) {
  useEffect(() => {
    let live = true;
    const stops: (() => void)[] = [];
    for (const event of [
      "alfredpet:assets",
      "alfredpet:settings",
      "alfredpet:modules",
    ])
      void on(event, () => {
        if (live) refresh();
      }).then((stop) => {
        if (live) stops.push(stop);
        else stop();
      });
    return () => {
      live = false;
      stops.forEach((stop) => stop());
    };
  }, [refresh]);
}

function useAsset(id: string) {
  const [asset, setAsset] = useState<Asset | null>(null);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const refresh = useCallback(() => {
    const current = ++generation.current;
    setAsset(null);
    setError("");
    if (!id) return;
    void request<Asset>("assets.get", { id })
      .then((value) => {
        if (generation.current === current) setAsset(value);
      })
      .catch((e) => {
        if (generation.current === current) setError(errorText(e));
      });
  }, [id]);
  useEffect(() => {
    refresh();
    return () => {
      ++generation.current;
    };
  }, [refresh]);
  useAssetEvents(refresh);
  return { asset, error, refresh };
}

export function ChatAssetImage({ id, alt }: { id: string; alt: string }) {
  const { asset, error } = useAsset(id);
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState("");
  const [failure, setFailure] = useState("");
  const [originalSize, setOriginalSize] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const generation = useRef(0);
  const titleId = useId();
  const close = useCallback(() => {
    generation.current++;
    dialog.current?.close();
    setOpen(false);
    setFull("");
    setFailure("");
    setOriginalSize(false);
  }, []);
  useAssetEvents(close);
  useEffect(() => {
    close();
    return () => {
      generation.current++;
    };
  }, [id, close]);
  useEffect(() => {
    if (open) dialog.current?.showModal();
  }, [open]);
  async function expand() {
    const current = ++generation.current;
    setOpen(true);
    setFull("");
    setFailure("");
    try {
      const image = await request<{ data_url: string }>("assets.image", { id });
      if (current !== generation.current) return;
      if (
        !/^data:image\/(png|jpeg|gif|webp);base64,[A-Za-z0-9+/]+=*$/.test(
          image.data_url,
        )
      )
        throw new Error("Imagen no disponible.");
      setFull(image.data_url);
    } catch (e) {
      if (current === generation.current) setFailure(errorText(e));
    }
  }
  const src = asset?.thumbnail_data_url;
  return src && /^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(src) ? (
    <>
      <button
        type="button"
        className="image-preview-button"
        aria-label={`Ver imagen: ${asset?.label || alt}`}
        title="Abrir imagen"
        aria-haspopup="dialog"
        onClick={() => void expand()}
      >
        <img
          className="chat-image"
          src={src}
          alt={alt || asset?.label || "Imagen de Alfred"}
        />
      </button>
      {open &&
        createPortal(
          <dialog
            ref={dialog}
            className="image-viewer"
            aria-labelledby={titleId}
            onCancel={(e) => {
              e.preventDefault();
              close();
            }}
            onClick={(e) => {
              if (e.target === e.currentTarget) close();
            }}
          >
            <div className="image-viewer-shell">
              <header>
                <strong id={titleId}>
                  {asset?.label || alt || "Imagen adjunta"}
                </strong>
                {full && (
                  <button
                    type="button"
                    onClick={() => setOriginalSize((value) => !value)}
                  >
                    {originalSize ? "Ajustar" : "Tamaño real"}
                  </button>
                )}
                <button type="button" autoFocus onClick={close}>
                  Cerrar imagen
                </button>
              </header>
              <div
                className={`image-viewer-content${originalSize ? " original-size" : ""}`}
              >
                {failure ? (
                  <p role="alert">{failure}</p>
                ) : full ? (
                  <img
                    src={full}
                    alt={alt || asset?.label || "Imagen adjunta"}
                    onError={() => {
                      setFull("");
                      setFailure("No se pudo mostrar la imagen.");
                    }}
                  />
                ) : (
                  <p role="status">Cargando imagen…</p>
                )}
              </div>
            </div>
          </dialog>,
          document.body,
        )}
    </>
  ) : (
    <span className="chat-image-unavailable">
      {alt || "Imagen"} · {error ? "imagen no disponible" : "cargando imagen…"}
    </span>
  );
}

function Thumbnail({ asset }: { asset: Asset }) {
  const src = asset.thumbnail_data_url;
  return src && /^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(src) ? (
    <img
      className="asset-thumbnail"
      src={src}
      alt={`Vista previa de ${asset.label}`}
    />
  ) : null;
}

export function CaptureAsset({
  id,
  onDeleted,
}: {
  id: string;
  onDeleted: () => void;
}) {
  const { asset, error } = useAsset(id);
  const [conversations, setConversations] = useState<
    { id: string; title: string }[]
  >([]);
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  useEffect(() => {
    let live = true;
    void request<{ id: string; title: string }[]>("conversations.list")
      .then((rows) => {
        if (live) setConversations(rows);
      })
      .catch((e) => {
        if (live) setActionError(errorText(e));
      });
    return () => {
      live = false;
    };
  }, []);
  useEffect(() => {
    setNotice("");
    setActionError("");
  }, [id]);
  async function action(remove: boolean) {
    setBusy(true);
    setActionError("");
    setNotice("");
    try {
      if (remove) {
        await request("assets.delete", { id });
        onDeleted();
      } else {
        const conversation =
          selected ||
          (await request<{ id: string }>("conversations.create")).id;
        await request("attachments.add", {
          conversation_id: conversation,
          asset_id: id,
        });
        setSelected(conversation);
        setNotice(
          "Captura adjuntada. Se conservará mientras siga en una conversación.",
        );
        const rows =
          await request<{ id: string; title: string }[]>("conversations.list");
        setConversations(rows);
      }
    } catch (e) {
      setActionError(errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <article className="card managed-asset">
      {(error || actionError) && <p role="alert">{actionError || error}</p>}
      {notice && <p role="status">{notice}</p>}
      {asset && (
        <>
          <Thumbnail asset={asset} />
          <p>
            {asset.width} × {asset.height} · {(asset.size / 1024).toFixed(1)}{" "}
            KiB
          </p>
          <p>
            {asset.reference_count > 0
              ? "Conservada en conversación"
              : asset.expires_at
                ? `Caduca: ${new Date(asset.expires_at).toLocaleString()}`
                : "Sin caducidad"}
          </p>
          <label>
            Adjuntar a conversación
            <select
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              disabled={busy}
            >
              <option value="">Nueva conversación</option>
              {conversations.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.title}
                </option>
              ))}
            </select>
          </label>
          <div className="row">
            <button disabled={busy} onClick={() => void action(false)}>
              Adjuntar captura
            </button>
            <button
              disabled={busy || asset.reference_count > 0}
              onClick={() => void action(true)}
            >
              Solicitar borrar captura
            </button>
          </div>
        </>
      )}
    </article>
  );
}

function AttachmentPreview({ id }: { id: string }) {
  return <ChatAssetImage id={id} alt="Imagen adjunta" />;
}
function attachmentLabel(row: Attachment) {
  try {
    const data = JSON.parse(row.metadata_json) as { label?: unknown };
    if (typeof data.label === "string" && data.label) return data.label;
  } catch {
    /* Older records may not contain presentation metadata. */
  }
  return row.path.split(/[\\/]/).pop() || "Adjunto";
}
export function useConversationAttachments(conversationId: string) {
  const [rows, setRows] = useState<Attachment[]>([]);
  const [loadedConversation, setLoadedConversation] = useState("");
  const [error, setError] = useState("");
  const generation = useRef(0);
  const refresh = useCallback(() => {
    const current = ++generation.current;
    setRows([]);
    if (!conversationId) return;
    void request<Attachment[]>("attachments.list", {
      conversation_id: conversationId,
    })
      .then((data) => {
        if (current === generation.current) {
          setRows(data);
          setLoadedConversation(conversationId);
          setError("");
        }
      })
      .catch((e) => {
        if (current === generation.current) setError(errorText(e));
      });
  }, [conversationId]);
  useEffect(() => {
    setError("");
    refresh();
    return () => {
      ++generation.current;
    };
  }, [refresh]);
  useAssetEvents(refresh);
  return {
    rows: loadedConversation === conversationId ? rows : [],
    error,
    reload: refresh,
  };
}
export function ConversationAttachments({
  conversationId,
  rows,
  busy = false,
  onChanged,
  sent = false,
}: {
  conversationId: string;
  rows: Attachment[];
  busy?: boolean;
  onChanged: () => void;
  sent?: boolean;
}) {
  const [error, setError] = useState("");
  const [removing, setRemoving] = useState(false);
  const [notice, setNotice] = useState("");
  useEffect(() => {
    setError("");
    setNotice("");
  }, [conversationId]);
  async function remove(row: Attachment) {
    setRemoving(true);
    setError("");
    setNotice("");
    try {
      const result = await request<{
        legacy_file_retained?: boolean;
        asset_retained_reason?: string;
      }>("attachments.remove", { conversation_id: conversationId, id: row.id });
      setNotice(
        result.asset_retained_reason
          ? `Adjunto retirado; copia conservada: ${result.asset_retained_reason}`
          : result.legacy_file_retained
            ? "Adjunto retirado. Su archivo original se conserva."
            : "Adjunto retirado de la conversación.",
      );
      onChanged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setRemoving(false);
    }
  }
  return (
    <div
      className={sent ? "message-attachments" : "conversation-attachments"}
      aria-label={
        sent
          ? "Adjuntos enviados con el mensaje"
          : "Adjuntos pendientes de enviar"
      }
    >
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {!sent && rows.length > 0 && <small>Pendientes de enviar</small>}
      {rows.map((row) => (
        <div className="attachment-card" key={row.id}>
          {row.asset_id && row.mime?.startsWith("image/") && (
            <AttachmentPreview id={row.asset_id} />
          )}
          <span>
            {attachmentLabel(row)} · {(row.size / 1024).toFixed(1)} KiB
          </span>
          <button disabled={busy || removing} onClick={() => void remove(row)}>
            Retirar de conversación
          </button>
        </div>
      ))}
    </div>
  );
}

export function AssetSettings() {
  const [minutes, setMinutes] = useState(1440);
  const [megabytes, setMegabytes] = useState(512);
  const [enabled, setEnabled] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [summary, setSummary] = useState("");
  useEffect(() => {
    let live = true;
    void request<{ key: string; json_value: string }[]>("settings.get")
      .then((rows) => {
        if (!live) return;
        const policy = rows.find((r) => r.key === "asset_lifecycle");
        if (policy) {
          const value = JSON.parse(policy.json_value) as {
            ttl_seconds: number;
            max_bytes: number;
            cleanup_enabled: boolean;
          };
          setMinutes(value.ttl_seconds / 60);
          setMegabytes(value.max_bytes / 1024 / 1024);
          setEnabled(value.cleanup_enabled);
        }
        const failure = rows.find((r) => r.key === "asset_lifecycle_error");
        if (failure && JSON.parse(failure.json_value))
          setSummary(
            `Limpieza pendiente de revisión: ${String(JSON.parse(failure.json_value))}`,
          );
        setLoaded(true);
      })
      .catch((e) => {
        if (live) setError(errorText(e));
      });
    return () => {
      live = false;
    };
  }, []);
  async function action(cleanup: boolean) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      if (cleanup) {
        const result = await tools<{
          expired: number;
          blocked: { id: string; error: string }[];
        }>("vision.cleanup");
        setNotice(
          `${result.expired} activos caducados eliminados; ${result.blocked.length} requieren revisión.`,
        );
        setSummary(result.blocked.map((r) => r.error).join(" · "));
      } else {
        if (
          !Number.isInteger(minutes) ||
          minutes < 5 ||
          minutes > 43200 ||
          !Number.isInteger(megabytes) ||
          megabytes < 64 ||
          megabytes > 2048
        )
          throw new Error("Indica de 5 a 43200 minutos y de 64 a 2048 MiB.");
        await request("settings.set", {
          key: "asset_lifecycle",
          value: {
            ttl_seconds: minutes * 60,
            max_bytes: megabytes * 1024 * 1024,
            cleanup_enabled: enabled,
          },
        });
        setNotice("Conservación guardada. Se aplicará a los nuevos activos.");
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <article className="card">
      <h2>Capturas y adjuntos</h2>
      <p>
        Los activos sin adjuntar caducan. Los adjuntos se conservan mientras
        estén en una conversación. La limpieza respeta los archivos originales.
      </p>
      <label>
        Conservación en minutos
        <input
          type="number"
          min={5}
          max={43200}
          value={minutes}
          onChange={(e) => setMinutes(Number(e.target.value))}
        />
      </label>
      <label>
        Almacenamiento máximo en MiB
        <input
          type="number"
          min={64}
          max={2048}
          value={megabytes}
          onChange={(e) => setMegabytes(Number(e.target.value))}
        />
      </label>
      <label>
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
        />
        Limpiar activos caducados al iniciar
      </label>
      <div className="row">
        <button disabled={!loaded || busy} onClick={() => void action(false)}>
          Guardar conservación
        </button>
        <button disabled={!loaded || busy} onClick={() => void action(true)}>
          Solicitar limpiar caducados
        </button>
      </div>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {summary && <p>{summary}</p>}
    </article>
  );
}
