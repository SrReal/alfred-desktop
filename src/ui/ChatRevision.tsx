import type { Message } from "../types";
export type Revision = {
  message: Message;
  mode: "edit" | "regenerate";
  text: string;
};

export function ChatRevision({
  value,
  busy,
  onChange,
  onCancel,
  onSubmit,
}: {
  value: Revision;
  busy: boolean;
  onChange: (text: string) => void;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <div className="chat-revision" role="region" aria-label="Revisar variante">
      <div className="chat-revision-content">
        <strong>
          {value.mode === "edit" ? "Editar y reenviar" : "Regenerar respuesta"}
        </strong>
        <p>
          Se creará una variante; el original seguirá en el historial. Repetir
          la petición puede volver a ejecutar acciones con tus permisos
          actuales.
        </p>
        {value.mode === "edit" ? (
          <textarea
            aria-label="Editar mensaje"
            value={value.text}
            disabled={busy}
            onChange={(e) => onChange(e.target.value)}
          />
        ) : (
          <p>
            Se repetirá la petición que originó esta respuesta, con el modelo
            seleccionado.
          </p>
        )}
        <small>
          Conserva los mensajes y adjuntos anteriores a la petición. Los
          recuerdos guardados se comparten según la configuración. No deshace
          acciones ya realizadas.
        </small>
      </div>
      <div className="row">
        <button disabled={busy} onClick={onCancel}>
          Cancelar variante
        </button>
        <button
          className="primary"
          disabled={busy || (value.mode === "edit" && !value.text.trim())}
          onClick={onSubmit}
        >
          {busy ? "Preparando…" : "Crear variante y enviar"}
        </button>
      </div>
    </div>
  );
}
