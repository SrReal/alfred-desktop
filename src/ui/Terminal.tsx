import { PathPicker } from "./PathPicker";
import { useEffect, useRef, useState } from "react";
import { on, request } from "../api";

type Profile = { id: string; name: string; shell: string };
type Output = { id: string; channel: string; text?: string; status?: string };
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function Terminal() {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [profile, setProfile] = useState("");
  const [elevated, setElevated] = useState(false);
  const [cwd, setCwd] = useState("");
  const [command, setCommand] = useState("");
  const [output, setOutput] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [running, setRunning] = useState(false);
  const [name, setName] = useState("");
  const [shell, setShell] = useState("auto");
  const [script, setScript] = useState("");
  const [envKeys, setEnvKeys] = useState("");
  const [subscribed, setSubscribed] = useState(false);
  const runId = useRef("");
  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void on<Output>("alfredpet:terminal", (event) => {
      if (!active || event.id !== runId.current) return;
      if (event.channel === "status") {
        setRunning(event.status === "running");
      } else {
        setOutput((previous) =>
          (previous + `[${event.channel}] ${event.text ?? ""}\n`).slice(
            -256000,
          ),
        );
      }
    })
      .then((stop) => {
        if (active) {
          unsubscribe = stop;
          setSubscribed(true);
        } else stop();
      })
      .catch((e) => {
        if (active) setError(message(e));
      });
    return () => {
      active = false;
      unsubscribe?.();
    };
  }, []);
  async function loadProfiles() {
    try {
      setProfiles(
        await request<Profile[]>("tools.execute", {
          tool: "terminal.profiles",
          input: {},
        }),
      );
      setError("");
    } catch (e) {
      setError(message(e));
    }
  }
  async function execute() {
    const id = `terminal-${crypto.randomUUID()}`;
    runId.current = id;
    setBusy(true);
    setOutput("");
    setError("");
    try {
      await request("tools.execute", {
        tool: "terminal.command",
        input: {
          id,
          command,
          cwd,
          elevated,
          ...(profile ? { profile_id: profile } : {}),
        },
      });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
      setRunning(false);
    }
  }
  async function saveProfile() {
    try {
      await request("tools.execute", {
        tool: "terminal.profile_save",
        input: {
          name,
          body: {
            shell,
            ...(script.trim() ? { script_path: script.trim() } : {}),
            env_allowlist: envKeys
              .split(",")
              .map((key) => key.trim())
              .filter(Boolean),
          },
        },
      });
      setName("");
      await loadProfiles();
    } catch (e) {
      setError(message(e));
    }
  }
  return (
    <section>
      <h2>Terminal</h2>
      <p>
        Activa Terminal en Configuración y elige una carpeta permitida. Las
        ejecuciones que necesitan permiso abren una solicitud con el comando
        completo.
      </p>
      <label className="toggle">
        <input
          type="checkbox"
          checked={elevated}
          disabled={busy}
          onChange={(e) => {
            setElevated(e.target.checked);
            if (e.target.checked) setProfile("");
          }}
        />
        Ejecutar como administrador (UAC)
      </label>
      {elevated && (
        <p>
          Windows PowerShell sin perfiles. Revisa el comando en Alfred y
          autoriza la elevación en Windows. No se eleva toda la aplicación.
        </p>
      )}
      <div className="row">
        <PathPicker
          label="Carpeta del terminal"
          mode="folder"
          value={cwd}
          disabled={busy}
          onChange={setCwd}
        />
        <select
          aria-label="Perfil del terminal"
          value={profile}
          disabled={busy || elevated}
          onChange={(e) => setProfile(e.target.value)}
        >
          <option value="">Sin perfil · PowerShell disponible</option>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name} · {p.shell}
            </option>
          ))}
        </select>
        <button disabled={busy} onClick={() => void loadProfiles()}>
          Cargar perfiles
        </button>
        {profile && (
          <button
            disabled={busy}
            onClick={() =>
              void request("tools.execute", {
                tool: "terminal.profile_delete",
                input: { id: profile },
              })
                .then(() => {
                  setProfile("");
                  void loadProfiles();
                })
                .catch((e) => setError(message(e)))
            }
          >
            Eliminar perfil
          </button>
        )}
      </div>
      <textarea
        aria-label="Comando PowerShell"
        placeholder="Get-Date"
        value={command}
        disabled={busy}
        onChange={(e) => setCommand(e.target.value)}
      />
      <div className="row">
        <button
          disabled={busy || !subscribed || !cwd.trim() || !command.trim()}
          onClick={() => void execute()}
        >
          Ejecutar comando
        </button>
        <button
          disabled={!running}
          onClick={() =>
            void request("tools.execute", {
              tool: "terminal.cancel",
              input: { id: runId.current },
            }).catch((e) => setError(message(e)))
          }
        >
          Cancelar comando
        </button>
        {busy && (
          <span role="status">
            {running
              ? "Ejecutando…"
              : "Preparando comando o esperando permiso…"}
          </span>
        )}
      </div>
      {error && <p role="alert">{error}</p>}
      <pre aria-label="Salida del terminal" aria-live="polite">
        {output}
      </pre>
      <details>
        <summary>Nuevo perfil</summary>
        <input
          aria-label="Nombre del perfil"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <select
          aria-label="PowerShell del perfil"
          value={shell}
          onChange={(e) => setShell(e.target.value)}
        >
          <option value="auto">Automático</option>
          <option value="powershell7">PowerShell 7</option>
          <option value="windows_powershell">Windows PowerShell 5.1</option>
        </select>
        <PathPicker
          label="Script del perfil"
          mode="file"
          value={script}
          onChange={setScript}
          extensions={["ps1"]}
        />
        <input
          aria-label="Variables permitidas del perfil"
          placeholder="Nombres separados por comas"
          value={envKeys}
          onChange={(e) => setEnvKeys(e.target.value)}
        />
        <button
          disabled={!name.trim() || busy}
          onClick={() => void saveProfile()}
        >
          Guardar perfil
        </button>
      </details>
    </section>
  );
}
