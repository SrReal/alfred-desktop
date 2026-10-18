import { useRef, useState } from "react";
import type { ReactNode } from "react";
import { request } from "../api";
import { PixelIcon } from "./PixelIcon";

const MAX_BYTES = 32 * 1024 * 1024;
function encoded(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`No se pudo leer ${file.name}.`));
    reader.onload = () => resolve(String(reader.result).split(",")[1]);
    reader.readAsDataURL(file);
  });
}
export function ChatFiles({
  busy,
  conversation,
  onBusy,
  onError,
  children,
  actions,
}: {
  busy: boolean;
  conversation: () => Promise<string>;
  onBusy: (busy: boolean) => void;
  onError: (error: string) => void;
  children: ReactNode;
  actions?: ReactNode;
}) {
  const picker = useRef<HTMLInputElement>(null);
  const importing = useRef(false);
  const [pending, setPending] = useState(false);
  const [over, setOver] = useState(false);
  const [notice, setNotice] = useState("");
  async function add(files: File[]) {
    if (!files.length) return;
    if (busy || importing.current) {
      onError(
        "Espera a que termine la operación actual para adjuntar archivos.",
      );
      return;
    }
    if (
      files.length > 5 ||
      files.some((f) => f.size === 0) ||
      files.reduce((n, f) => n + f.size, 0) > MAX_BYTES
    ) {
      onError(
        "Adjunta hasta 5 archivos, con un máximo de 32 MiB en total y sin archivos vacíos.",
      );
      return;
    }
    importing.current = true;
    setPending(true);
    onBusy(true);
    onError("");
    setNotice("");
    let done = 0;
    try {
      const id = await conversation();
      for (const file of files) {
        await request("attachments.import", {
          conversation_id: id,
          name: file.name || "captura.png",
          data_base64: await encoded(file),
        });
        done++;
      }
      setNotice(
        `${done === 1 ? "Archivo adjuntado" : `${done} archivos adjuntados`}.`,
      );
    } catch (error) {
      onError(
        `${done ? `${done} archivo(s) adjuntado(s). ` : ""}${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      importing.current = false;
      setPending(false);
      onBusy(false);
    }
  }
  return (
    <div
      className={`chat-files${over ? " drag-over" : ""}`}
      onPaste={(e) => {
        const files = Array.from(e.clipboardData.files);
        if (files.length) {
          e.preventDefault();
          void add(files);
        }
      }}
      onDragOver={(e) => {
        if (!Array.from(e.dataTransfer.types).includes("Files")) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = busy || pending ? "none" : "copy";
        setOver(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null))
          setOver(false);
      }}
      onDrop={(e) => {
        if (!Array.from(e.dataTransfer.types).includes("Files")) return;
        e.preventDefault();
        setOver(false);
        void add(Array.from(e.dataTransfer.files));
      }}
    >
      {children}
      <div className="file-actions">
        <input
          ref={picker}
          type="file"
          multiple
          hidden
          aria-label="Seleccionar archivos"
          onChange={(e) => {
            void add(Array.from(e.currentTarget.files ?? []));
            e.currentTarget.value = "";
          }}
        />
        <button
          disabled={busy || pending}
          onClick={() => picker.current?.click()}
        >
          <PixelIcon name="attachment" />{" "}
          {pending ? "Adjuntando…" : "Adjuntar archivos"}
        </button>
        <span
          className="file-help"
          title="Arrastra archivos o pega una captura · hasta 32 MiB"
        >
          {over
            ? "Suelta los archivos aquí"
            : "Arrastra archivos o pega una captura · hasta 32 MiB"}
        </span>
        {actions}
      </div>
      {notice && (
        <p className="file-notice" role="status">
          {notice}
        </p>
      )}
    </div>
  );
}
