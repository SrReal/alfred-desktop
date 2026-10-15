import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";

type State = {
  phase?: string;
  session_id?: string;
  error?: string;
  cleanup_pending?: boolean;
  picked_file_ids?: string[];
};
type Event = { connection: string; session_id: string };
const phases: Record<string, string> = {
  authorization_pending: "Preparando la autorización de Drive",
  browser_launch_in_flight: "Abriendo el consentimiento de Drive",
  status_unavailable: "Estado de Drive no disponible",
  authorizing: "Esperando el consentimiento en el navegador del sistema",
  exchange_in_flight: "Guardando la autorización",
  ready: "Drive autorizado",
  reauthorize: "Es necesario autorizar Drive",
  uncertain: "Resultado incierto: vuelve a autorizar Drive",
  cancelling: "Cancelando la autorización",
  refresh_in_flight: "Renovando la autorización",
};
export function DriveAuthorization({ connection }: { connection: string }) {
  const [state, setState] = useState<State>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const epoch = useRef(0);
  const current = useRef<State>({});
  const pending = useRef(false);
  const refresh = useRef<(ticket: number, session?: string) => Promise<void>>(
    async () => {},
  );
  refresh.current = async (ticket: number, session?: string) => {
    try {
      const result = await request<State | null>("connections.oauth_status", {
        connection,
      });
      if (epoch.current !== ticket) return;
      if (session && result?.session_id && result.session_id !== session) {
        current.current = {
          phase: "reauthorize",
          error: "DRIVE_AUTH_SUPERSEDED",
        };
        setState(current.current);
        return;
      }
      current.current = result ?? {};
      setState(current.current);
    } catch (e) {
      if (epoch.current === ticket) {
        setError(String(e));
        current.current = { ...current.current, phase: "status_unavailable" };
        setState(current.current);
      }
    }
  };
  useEffect(() => {
    const ticket = epoch.current;
    void refresh.current(ticket);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void on<Event>("alfredpet:oauth", (event) => {
      if (
        !disposed &&
        event.connection === connection &&
        event.session_id === current.current.session_id
      ) {
        void refresh.current(epoch.current, event.session_id);
      }
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      epoch.current++;
      unlisten?.();
    };
  }, [connection]);
  useEffect(() => {
    if (
      ![
        "authorizing",
        "authorization_pending",
        "browser_launch_in_flight",
        "exchange_in_flight",
        "cancelling",
        "refresh_in_flight",
      ].includes(state.phase ?? "")
    )
      return;
    const ticket = epoch.current;
    const timer = setInterval(() => {
      if (pending.current) return;
      pending.current = true;
      void refresh.current(ticket, state.session_id).finally(() => {
        pending.current = false;
      });
    }, 1000);
    return () => clearInterval(timer);
  }, [state.phase, state.session_id]);
  async function start(pick = false) {
    if (busy) return;
    const ticket = ++epoch.current;
    setBusy(true);
    setError("");
    try {
      const result = await request<{ session_id: string }>(
        "connections.oauth",
        {
          connection,
          ...(pick ? { pick_resources: true } : {}),
        },
      );
      if (epoch.current !== ticket) return;
      current.current = { phase: "authorizing", session_id: result.session_id };
      setState(current.current);
      await refresh.current(ticket, result.session_id);
    } catch (e) {
      if (epoch.current === ticket) {
        setError(String(e));
        current.current = { ...current.current, phase: "status_unavailable" };
        setState(current.current);
      }
    } finally {
      if (epoch.current === ticket) setBusy(false);
    }
  }
  async function cancel() {
    const session = current.current.session_id;
    if (!session || busy) return;
    const ticket = ++epoch.current;
    setBusy(true);
    setError("");
    current.current = { ...current.current, phase: "cancelling" };
    setState(current.current);
    try {
      await request("connections.oauth_cancel", {
        connection,
        session_id: session,
      });
      if (epoch.current !== ticket) return;
      await refresh.current(ticket, session);
    } catch (e) {
      if (epoch.current === ticket) setError(String(e));
    } finally {
      if (epoch.current === ticket) setBusy(false);
    }
  }
  const active = [
    "authorization_pending",
    "browser_launch_in_flight",
    "authorizing",
    "exchange_in_flight",
    "cancelling",
  ].includes(state.phase ?? "");
  return (
    <div>
      <p>
        Volver a autorizar invalida las subidas pendientes de este perfil. En el
        selector de Google puedes elegir hasta 100 recursos.
      </p>
      <button disabled={busy} onClick={() => void start()}>
        Autorizar Google Drive
      </button>
      <button disabled={busy} onClick={() => void start(true)}>
        Autorizar y elegir archivos o carpetas
      </button>
      {active && (
        <button disabled={busy} onClick={() => void cancel()}>
          Cancelar autorización de Drive
        </button>
      )}
      <p role="status">
        {phases[state.phase ?? ""] ?? "Drive pendiente de autorización"}
      </p>
      {state.cleanup_pending && (
        <p>Quedan credenciales anteriores pendientes de limpieza.</p>
      )}
      {!!state.picked_file_ids?.length && (
        <div>
          <p>
            IDs elegidos en Google. Añade los archivos a allowed_files y las
            carpetas a allowed_folders en la configuración, y guarda para
            permitir su uso en AlfredPet. El permiso de Google no añade estos
            IDs automáticamente.
          </p>
          <textarea
            aria-label="IDs elegidos en Google Drive"
            readOnly
            value={JSON.stringify(state.picked_file_ids, null, 2)}
          />
        </div>
      )}
      {(error || state.error) && <p role="alert">{error || state.error}</p>}
    </div>
  );
}
