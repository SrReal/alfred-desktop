import { useEffect, useRef, useState } from "react";
import { request } from "../api";
import type { Json } from "../types";
import { McpSubscriptions } from "./McpSubscriptions";
type Kind = "tools" | "resources" | "prompts";
type Entry = Record<string, Json> & {
  name: string;
  uri?: string;
  alfredpetPermission: string;
  alfredpetObservation?: { fingerprint: string };
};
type Profile = Record<string, Json> & {
  permissions?: {
    tools?: Record<string, boolean>;
    resources?: string[];
    prompts?: Record<string, boolean>;
  };
};
export function McpInspector({
  connection,
  name,
  profile,
  onSaved,
}: {
  connection: string;
  name: string;
  profile: Json;
  onSaved: (profile: Json) => void;
}) {
  const [kind, setKind] = useState<Kind>("tools");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [selected, setSelected] = useState<Entry | null>(null);
  const [body, setBody] = useState("{}");
  const [output, setOutput] = useState<Json | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [issuer, setIssuer] = useState("");
  const [metadataUrl, setMetadataUrl] = useState("");
  const [discovery, setDiscovery] = useState<Record<string, Json> | null>(null);
  const [clientMode, setClientMode] = useState("manual");
  const [clientId, setClientId] = useState("");
  const [redirectUri, setRedirectUri] = useState(
    "http://127.0.0.1/oauth/mcp/callback",
  );
  const [documentUrl, setDocumentUrl] = useState("");
  const [clientAuth, setClientAuth] = useState("none");
  const [scopes, setScopes] = useState("");
  const [omitScopes, setOmitScopes] = useState(false);
  const [clientSecret, setClientSecret] = useState("");
  const [oauthState, setOauthState] = useState<Record<string, Json> | null>(
    null,
  );
  const statusEpoch = useRef(0);
  const [acknowledgeUnknown, setAcknowledgeUnknown] = useState(false);
  useEffect(() => {
    setAcknowledgeUnknown(false);
  }, [oauthState?.session_id]);
  const auth = (profile as Record<string, Json>).auth as
    Record<string, Json> | undefined;
  const oauthProfile = auth?.type === "oauth";
  const savedIssuer = typeof auth?.issuer === "string" ? auth.issuer : "";
  const pending = useRef(false);
  const epoch = useRef(0);
  const seen = useRef(new Set<string>());
  useEffect(
    () => () => {
      epoch.current++;
    },
    [],
  );
  useEffect(() => {
    epoch.current++;
    setDiscovery(null);
    setOutput(null);
    setEntries([]);
    setSelected(null);
    setCursor(null);
    setBusy(false);
    setClientSecret("");
    setOauthState(null);
    setAcknowledgeUnknown(false);
    statusEpoch.current++;
    if (oauthProfile) setIssuer(savedIssuer);
    seen.current.clear();
  }, [connection, profile, oauthProfile, savedIssuer]);
  useEffect(() => {
    setClientSecret("");
  }, [clientId, clientMode, clientAuth, issuer, redirectUri, metadataUrl]);
  useEffect(() => {
    if (!oauthProfile) return;
    let stopped = false;
    let active = false;
    async function poll() {
      if (active || pending.current) return;
      active = true;
      const ticket = statusEpoch.current;
      try {
        const value = await request<Record<string, Json>>(
          "connections.mcp_oauth_status",
          { connection },
        );
        if (
          !stopped &&
          ticket === statusEpoch.current &&
          value.connection === connection
        )
          setOauthState(value);
      } catch {
        if (!stopped && ticket === statusEpoch.current) setOauthState(null);
      } finally {
        active = false;
      }
    }
    void poll();
    const interval = window.setInterval(() => void poll(), 2000);
    return () => {
      stopped = true;
      window.clearInterval(interval);
    };
  }, [connection, profile, oauthProfile]);
  function registrationInput(): Record<string, Json> {
    return {
      issuer: discovery?.issuer ?? issuer,
      expected: discovery?.alfredpetObservation ?? null,
      client_auth: clientAuth,
      requested_scopes: omitScopes
        ? null
        : scopes
            .split(/\r?\n/)
            .map((s) => s.trim())
            .filter(Boolean),
      ...(clientMode === "pre_registered"
        ? {
            preregistered: {
              client_id: clientId.trim(),
              redirect_uri: redirectUri.trim(),
            },
          }
        : {}),
      ...(clientMode === "client_metadata_document"
        ? { metadata_document_url: documentUrl.trim() }
        : {}),
      allow_legacy_dynamic: clientMode === "legacy_dynamic",
    };
  }
  async function saveOAuth() {
    if (pending.current) return;
    pending.current = true;
    const ticket = epoch.current;
    statusEpoch.current++;
    setBusy(true);
    setError("");
    setClientSecret("");
    try {
      const updated = {
        ...(profile as Record<string, Json>),
        auth: { type: "oauth", issuer: issuer.trim() },
      };
      await request("connections.save", {
        id: connection,
        module_id: "mcp",
        name,
        config: updated,
        expected_profile: profile,
      });
      if (ticket === epoch.current) {
        setDiscovery(null);
        setOutput(null);
        onSaved(updated);
      }
    } catch (e) {
      if (ticket === epoch.current)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      pending.current = false;
      if (ticket === epoch.current) setBusy(false);
    }
  }
  async function run(action: string, input: Record<string, Json>) {
    if (pending.current) return;
    pending.current = true;
    const ticket = epoch.current;
    statusEpoch.current++;
    setBusy(true);
    setError("");
    setOutput(null);
    try {
      const result =
        action === "oauth_start"
          ? await request<Record<string, Json>>("connections.mcp_oauth", {
              input: { connection, ...input },
              client_secret:
                clientMode === "pre_registered" &&
                clientAuth !== "none" &&
                clientSecret
                  ? clientSecret
                  : null,
            })
          : await request<Record<string, Json>>("tools.execute", {
              tool: `mcp.${action}`,
              input: { connection, ...input },
            });
      if (ticket !== epoch.current) return;
      if (
        [
          "oauth_start",
          "oauth_cancel",
          "oauth_refresh",
          "oauth_recover",
          "oauth_cleanup",
          "oauth_reset",
        ].includes(action)
      ) {
        setOauthState(result);
        setAcknowledgeUnknown(false);
      }
      if (action === "oauth_discover") {
        const owned = result.alfredpetObservation as
          Record<string, Json> | undefined;
        if (
          typeof owned?.fingerprint === "string" &&
          /^[a-f0-9]{64}$/.test(owned.fingerprint) &&
          result.issuer === input.issuer
        ) {
          setDiscovery(result);
          const proposed = result.requested_scopes;
          setScopes(
            Array.isArray(proposed) &&
              proposed.every((s) => typeof s === "string")
              ? proposed.join("\n")
              : "",
          );
          setOmitScopes(proposed === null);
        } else setDiscovery(null);
      }
      if (action === kind) {
        const items = result[kind];
        if (!Array.isArray(items))
          throw new Error("La lista MCP no es válida.");
        const next = result.nextCursor;
        if (
          next !== undefined &&
          (typeof next !== "string" || !next || seen.current.has(next))
        )
          throw new Error("La paginación MCP se repitió o no es válida.");
        if (typeof next === "string") seen.current.add(next);
        setEntries(items as Entry[]);
        setCursor(typeof next === "string" ? next : null);
        setSelected(null);
      } else setOutput(result);
    } catch (e) {
      if (ticket === epoch.current) {
        setError(e instanceof Error ? e.message : String(e));
        setEntries([]);
        setCursor(null);
        setSelected(null);
        if (
          ["oauth_discover", "oauth_registration", "oauth_start"].includes(
            action,
          )
        )
          setDiscovery(null);
      }
    } finally {
      pending.current = false;
      if (action === "oauth_start") setClientSecret("");
      if (ticket === epoch.current) setBusy(false);
    }
  }
  async function permission(entry: Entry) {
    if (pending.current) return;
    pending.current = true;
    const ticket = epoch.current;
    setBusy(true);
    setError("");
    try {
      const current = profile as Profile;
      const permissions = {
        tools: {},
        resources: [],
        prompts: {},
        ...current.permissions,
      };
      const subject = kind === "resources" ? entry.uri! : entry.name;
      const allow = entry.alfredpetPermission !== "allowed";
      if (kind === "resources")
        permissions.resources = allow
          ? [...new Set([...permissions.resources, subject])]
          : permissions.resources.filter((uri) => uri !== subject);
      else permissions[kind] = { ...permissions[kind], [subject]: allow };
      const updated = { ...current, permissions };
      await request("connections.save", {
        id: connection,
        module_id: "mcp",
        name,
        config: updated,
        expected_profile: profile,
      });
      if (ticket !== epoch.current) return;
      setEntries([]);
      setCursor(null);
      setSelected(null);
      setOutput(null);
      seen.current.clear();
      setDiscovery(null);
      onSaved(updated);
    } catch (e) {
      if (ticket === epoch.current)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      pending.current = false;
      if (ticket === epoch.current) setBusy(false);
    }
  }
  function execute() {
    if (
      !selected?.alfredpetObservation ||
      selected.alfredpetPermission !== "allowed"
    )
      return;
    try {
      const action =
        kind === "tools"
          ? "call"
          : kind === "resources"
            ? "read_resource"
            : "get_prompt";
      const input: Record<string, Json> = {
        expected: selected.alfredpetObservation,
      };
      if (kind === "resources") input.uri = selected.uri!;
      else {
        input.name = selected.name;
        input.body = JSON.parse(body) as Json;
      }
      void run(action, input);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }
  return (
    <div className="card">
      <h2>Servidor MCP</h2>
      <p>
        Consultar el servidor requiere aprobación. Sus descripciones son
        contenido externo. Permitir un elemento guarda tu elección; cada
        ejecución requiere una aprobación nueva.
      </p>
      <button disabled={busy} onClick={() => void run("health", {})}>
        Comprobar servidor MCP
      </button>
      {(profile as Record<string, Json>).protocol !== "legacy" && (
        <McpSubscriptions
          connection={connection}
          profile={profile}
          onChanged={() => {
            epoch.current++;
            setBusy(false);
            setEntries([]);
            setSelected(null);
            setCursor(null);
            setOutput(null);
            seen.current.clear();
          }}
        />
      )}
      {(profile as Record<string, Json>).transport === "streamable_http" && (
        <div className="card">
          <h3>Revisar autorización del servidor</h3>
          <p>
            Indica el emisor HTTPS que quieres consultar. La consulta requiere
            aprobación y muestra sus destinos y permisos. Guarda el emisor en
            esta conexión antes de iniciar sesión en el navegador.
          </p>
          <label>
            Emisor OAuth MCP
            <input
              aria-label="Emisor OAuth MCP"
              value={issuer}
              disabled={busy}
              onChange={(e) => {
                setIssuer(e.target.value);
                setOutput(null);
                setDiscovery(null);
              }}
            />
          </label>
          {(!oauthProfile || auth?.issuer !== issuer.trim()) && (
            <button
              disabled={busy || !issuer.trim()}
              onClick={() => void saveOAuth()}
            >
              Guardar emisor OAuth
            </button>
          )}
          {oauthProfile && (
            <div aria-live="polite">
              <p>
                {oauthState?.phase === "ready"
                  ? oauthState.credential_status === "usable"
                    ? "Sesión OAuth disponible"
                    : oauthState.credential_status === "expired"
                      ? "Sesión OAuth caducada"
                      : "Comprobando credencial OAuth"
                  : oauthState?.phase === "authorizing"
                    ? "Esperando autorización en el navegador"
                    : [
                          "uncertain",
                          "publication_pending",
                          "registration_in_flight",
                          "exchange_in_flight",
                          "refresh_in_flight",
                        ].includes(String(oauthState?.phase))
                      ? "Autorización pendiente; verifica el estado antes de repetir"
                      : oauthState?.phase === "cancelled"
                        ? "Autorización cancelada"
                        : oauthState?.phase === "client_registered"
                          ? "Cliente registrado; falta autorizar la sesión"
                          : "Sin sesión OAuth disponible"}
              </p>
              {typeof oauthState?.session_id === "string" && (
                <>
                  {oauthState.phase === "authorizing" && (
                    <button
                      disabled={busy}
                      onClick={() =>
                        void run("oauth_cancel", {
                          session_id: oauthState.session_id,
                        })
                      }
                    >
                      Cancelar autorización MCP
                    </button>
                  )}
                  {oauthState.phase === "ready" && (
                    <button
                      disabled={busy}
                      onClick={() =>
                        void run("oauth_refresh", {
                          session_id: oauthState.session_id,
                        })
                      }
                    >
                      Renovar sesión MCP
                    </button>
                  )}
                  {["uncertain", "publication_pending"].includes(
                    String(oauthState.phase),
                  ) && (
                    <button
                      disabled={busy}
                      onClick={() =>
                        void run("oauth_recover", {
                          session_id: oauthState.session_id,
                        })
                      }
                    >
                      Recuperar credencial guardada MCP
                    </button>
                  )}
                  {oauthState.cleanup_pending === true && (
                    <button
                      disabled={busy}
                      onClick={() =>
                        void run("oauth_cleanup", {
                          session_id: oauthState.session_id,
                        })
                      }
                    >
                      Limpiar recibos antiguos MCP
                    </button>
                  )}
                  {[
                    "uncertain",
                    "registration_in_flight",
                    "exchange_in_flight",
                    "refresh_in_flight",
                    "publication_pending",
                  ].includes(String(oauthState.phase)) && (
                    <div>
                      <label>
                        <input
                          type="checkbox"
                          aria-label="Aceptar desenlace OAuth desconocido MCP"
                          disabled={busy}
                          checked={acknowledgeUnknown}
                          onChange={(e) =>
                            setAcknowledgeUnknown(e.target.checked)
                          }
                        />
                        Reconozco que el servidor puede haber completado la
                        operación. Un nuevo registro podría crear otro cliente.
                      </label>
                      <button
                        disabled={busy || !acknowledgeUnknown}
                        onClick={() =>
                          void run("oauth_reset", {
                            session_id: oauthState.session_id,
                            acknowledge_remote_unknown: true,
                          })
                        }
                      >
                        Cerrar intento OAuth desconocido MCP
                      </button>
                    </div>
                  )}
                </>
              )}
            </div>
          )}
          <label>
            URL de metadatos del recurso (opcional)
            <input
              aria-label="URL de metadatos OAuth MCP"
              value={metadataUrl}
              disabled={busy}
              onChange={(e) => {
                setMetadataUrl(e.target.value);
                setOutput(null);
                setDiscovery(null);
              }}
            />
          </label>
          <button
            disabled={busy || !issuer.trim()}
            onClick={() =>
              void run("oauth_discover", {
                issuer: issuer.trim(),
                ...(metadataUrl.trim()
                  ? { metadata_url: metadataUrl.trim() }
                  : {}),
              })
            }
          >
            Consultar autorización MCP
          </button>
          {discovery && (
            <div>
              <h3>Revisar el cliente y sus permisos</h3>
              <p>
                Elige cómo registrar AlfredPet. Esta revisión muestra el plan;
                iniciar sesión requiere otra aprobación para registrar el
                cliente cuando corresponda y abrir el navegador.
              </p>
              <label>
                Registro del cliente
                <select
                  aria-label="Registro del cliente MCP"
                  disabled={busy}
                  value={clientMode}
                  onChange={(e) => {
                    setClientMode(e.target.value);
                    setOutput(null);
                  }}
                >
                  <option value="manual">Registro manual</option>
                  <option value="pre_registered">Cliente ya registrado</option>
                  <option value="client_metadata_document">
                    Documento del cliente
                  </option>
                  <option value="legacy_dynamic">
                    Registro dinámico heredado
                  </option>
                </select>
              </label>
              {clientMode === "pre_registered" && clientAuth !== "none" && (
                <label>
                  Secreto del cliente
                  <input
                    aria-label="Secreto del cliente MCP"
                    type="password"
                    autoComplete="off"
                    disabled={busy}
                    value={clientSecret}
                    onChange={(e) => setClientSecret(e.target.value)}
                  />
                  <small>
                    Puedes dejarlo vacío para reutilizar un secreto guardado de
                    este cliente.
                  </small>
                </label>
              )}
              {clientMode === "pre_registered" && (
                <>
                  <label>
                    Identificador del cliente
                    <input
                      aria-label="Identificador del cliente MCP"
                      disabled={busy}
                      value={clientId}
                      onChange={(e) => {
                        setClientId(e.target.value);
                        setOutput(null);
                      }}
                    />
                  </label>
                  <label>
                    Dirección de retorno registrada
                    <input
                      aria-label="Dirección de retorno MCP"
                      disabled={busy}
                      value={redirectUri}
                      onChange={(e) => {
                        setRedirectUri(e.target.value);
                        setOutput(null);
                      }}
                    />
                  </label>
                </>
              )}
              {clientMode === "client_metadata_document" && (
                <label>
                  URL del documento del cliente
                  <input
                    aria-label="Documento del cliente MCP"
                    disabled={busy}
                    value={documentUrl}
                    onChange={(e) => {
                      setDocumentUrl(e.target.value);
                      setOutput(null);
                    }}
                  />
                </label>
              )}
              <label>
                Autenticación del cliente
                <select
                  aria-label="Autenticación del cliente MCP"
                  disabled={busy}
                  value={clientAuth}
                  onChange={(e) => {
                    setClientAuth(e.target.value);
                    setOutput(null);
                  }}
                >
                  <option value="none">Cliente público</option>
                  <option value="client_secret_basic">
                    Secreto en cabecera
                  </option>
                  <option value="client_secret_post">
                    Secreto en formulario
                  </option>
                </select>
              </label>
              <label>
                <input
                  type="checkbox"
                  aria-label="Usar permisos predeterminados MCP"
                  disabled={busy}
                  checked={omitScopes}
                  onChange={(e) => {
                    setOmitScopes(e.target.checked);
                    setOutput(null);
                  }}
                />
                Usar permisos predeterminados del servidor
              </label>
              {!omitScopes && (
                <label>
                  Permisos solicitados, uno por línea
                  <textarea
                    aria-label="Permisos OAuth MCP"
                    disabled={busy}
                    value={scopes}
                    onChange={(e) => {
                      setScopes(e.target.value);
                      setOutput(null);
                    }}
                    spellCheck={false}
                  />
                </label>
              )}
              <button
                disabled={
                  busy ||
                  (!omitScopes && !scopes.trim()) ||
                  (clientMode === "pre_registered" &&
                    (!clientId.trim() || !redirectUri.trim())) ||
                  (clientMode === "client_metadata_document" &&
                    !documentUrl.trim())
                }
                onClick={() =>
                  void run("oauth_registration", registrationInput())
                }
              >
                Revisar registro MCP
              </button>
              {oauthProfile && (
                <button
                  disabled={
                    busy ||
                    auth?.issuer !== discovery.issuer ||
                    clientMode === "manual" ||
                    (!omitScopes && !scopes.trim()) ||
                    (clientMode === "pre_registered" &&
                      (!clientId.trim() || !redirectUri.trim())) ||
                    (clientMode === "client_metadata_document" &&
                      !documentUrl.trim()) ||
                    [
                      "authorizing",
                      "registration_in_flight",
                      "exchange_in_flight",
                      "refresh_in_flight",
                      "publication_pending",
                      "uncertain",
                    ].includes(String(oauthState?.phase))
                  }
                  onClick={() => void run("oauth_start", registrationInput())}
                >
                  Iniciar sesión MCP
                </button>
              )}
            </div>
          )}
        </div>
      )}
      <select
        aria-label="Lista MCP"
        value={kind}
        disabled={busy}
        onChange={(e) => {
          epoch.current++;
          setKind(e.target.value as Kind);
          setEntries([]);
          setCursor(null);
          setSelected(null);
          setOutput(null);
          seen.current.clear();
        }}
      >
        <option value="tools">Herramientas</option>
        <option value="resources">Recursos</option>
        <option value="prompts">Prompts</option>
      </select>
      <button
        disabled={busy}
        onClick={() => {
          seen.current.clear();
          void run(kind, {});
        }}
      >
        Consultar lista MCP
      </button>
      {cursor && (
        <button disabled={busy} onClick={() => void run(kind, { cursor })}>
          Página siguiente MCP
        </button>
      )}
      {entries.map((entry) => (
        <article key={entry.uri ?? entry.name} className="card">
          <strong>{entry.name}</strong> <code>{entry.uri}</code>
          <p>
            {entry.alfredpetPermission === "allowed"
              ? "Permitido por ti"
              : "Denegado"}
          </p>
          <button disabled={busy} onClick={() => void permission(entry)}>
            {entry.alfredpetPermission === "allowed" ? "Denegar" : "Permitir"}{" "}
            {entry.name}
          </button>
          {entry.alfredpetObservation &&
            entry.alfredpetPermission === "allowed" && (
              <button
                disabled={busy}
                onClick={() => {
                  setSelected(entry);
                  setBody("{}");
                  setOutput(null);
                }}
              >
                Preparar {entry.name}
              </button>
            )}
        </article>
      ))}
      {selected && (
        <div>
          <p>Revisar {selected.name}</p>
          {kind !== "resources" && (
            <textarea
              aria-label="Argumentos MCP"
              disabled={busy}
              value={body}
              onChange={(e) => setBody(e.target.value)}
              spellCheck={false}
            />
          )}
          <button disabled={busy} onClick={execute}>
            Solicitar ejecución MCP
          </button>
        </div>
      )}
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {output !== null && (
        <div>
          <p>Respuesta del servidor · contenido externo</p>
          <pre>{JSON.stringify(output, null, 2)}</pre>
        </div>
      )}
    </div>
  );
}
