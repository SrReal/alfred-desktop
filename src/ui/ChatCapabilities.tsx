import { useState } from "react";
import { PixelIcon } from "./PixelIcon";

export function ChatCapabilities({
  busy,
  onDraft,
}: {
  busy: boolean;
  onDraft: (text: string) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="chat-capabilities">
      <button type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        <PixelIcon name="plugin" /> Skills y plugins
      </button>
      {open && (
        <div className="capabilities-panel">
          <p>
            Pide a Alfred una skill con instrucciones reutilizables o un plugin
            con nuevas herramientas. Puedes crearlos y mejorarlos aquí;
            revisarás los cambios antes de guardarlos.
          </p>
          <div className="row">
            {[
              ["Nueva skill", "Quiero crear una skill reutilizable para: "],
              ["Mejorar skill", "Lista mis skills y ayúdame a mejorar una."],
              ["Nuevo plugin", "Quiero crear un plugin para: "],
              [
                "Mejorar plugin",
                "Consulta mis plugins y ayúdame a mejorar uno, conservando la versión anterior.",
              ],
            ].map(([label, text]) => (
              <button
                key={label}
                disabled={busy}
                onClick={() => {
                  onDraft(text);
                  setOpen(false);
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <small>
            Skills, Custom y Plugin Builder deben estar activos en Configuración
            → Módulos. Los plugins pasan pruebas antes de habilitarse.
          </small>
        </div>
      )}
    </div>
  );
}
