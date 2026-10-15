import { useState } from "react";

export function ChatCopyButton({
  text,
  label = "Copiar mensaje",
}: {
  text: string | (() => string);
  label?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");
  const [busy, setBusy] = useState(false);
  return (
    <span className="chat-copy">
      <button
        type="button"
        disabled={busy}
        aria-label={label}
        onClick={async () => {
          setBusy(true);
          try {
            await navigator.clipboard.writeText(
              typeof text === "function" ? text() : text,
            );
            setState("copied");
          } catch {
            setState("error");
          } finally {
            setBusy(false);
          }
        }}
      >
        {state === "copied"
          ? "Copiado"
          : label === "Copiar código"
            ? "Copiar código"
            : "Copiar"}
      </button>
      {state === "error" && (
        <span role="alert">
          No se pudo copiar. Selecciona el texto y pulsa Ctrl+C.
        </span>
      )}
    </span>
  );
}
