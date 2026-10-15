import { PathPicker } from "./PathPicker";
import { useCallback, useEffect, useRef, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";
type Summary = {
  id: string;
  name: string;
  version: string;
  state: string;
  runtime: string;
};
type Detail = {
  id: string;
  manifest: {
    name: string;
    version: string;
    description: string;
    sideEffects: string[];
    rollback: string;
    inputSchema: Json;
    outputSchema: Json;
    permissions: Json;
    secrets: string[];
  };
  path: string;
  state: string;
  fingerprint: string;
  tests: Json;
  health?: string;
  error_code?: string | null;
  isolation: string;
};
const call = <T,>(tool: string, input: Json = {}) =>
  request<T>("tools.execute", { tool, input });
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
export function sampleProposal(): Json {
  return {
    manifest: {
      id: "custom.reviewed-echo",
      name: "Eco revisado",
      version: "1.0.0",
      type: "custom",
      runtime: "node",
      entrypoint: "index.mjs",
      test: "test.mjs",
      description: "Devuelve el texto recibido",
      inputSchema: {
        type: "object",
        properties: { text: { type: "string", maxLength: 1000 } },
        required: ["text"],
        additionalProperties: false,
      },
      outputSchema: {
        type: "object",
        properties: { text: { type: "string" } },
        required: ["text"],
        additionalProperties: false,
      },
      permissions: { defaultRisk: "mandatory", network: false, filesystem: [] },
      timeoutMs: 5000,
      sideEffects: [
        "Ejecuta un proceso Node local y devuelve texto; no necesita escribir archivos ni red.",
      ],
      rollback: "No requiere una operación inversa para el eco.",
      secrets: [],
      fixtures: { happy: { text: "owned fixture" }, badInput: {} },
    },
    files: {
      "index.mjs":
        "export function run(input){if(typeof input.text!=='string')throw new Error('INVALID_INPUT');return {text:input.text};}\nif(!process.env.ALFREDPET_TEST_CASE){let data='';for await(const chunk of process.stdin)data+=chunk;process.stdout.write(JSON.stringify(run(JSON.parse(data))));}\n",
      "test.mjs":
        "if(process.env.ALFREDPET_TEST_CASE==='timeout'){await new Promise(resolve=>setTimeout(resolve,10000));}else{const assert=await import('node:assert/strict');const {run}=await import('./index.mjs');assert.deepEqual(run({text:'ok'}),{text:'ok'});assert.throws(()=>run({}));}\n",
      "README.md":
        "# AlfredPet eco\n\nEntrada/salida: objeto text. Sin red ni escrituras necesarias. Cada ejecución requiere aprobación. Revisar el código antes de probarlo; es código local con acceso a los recursos del usuario.\n",
      "icon.svg":
        "<svg xmlns='http://www.w3.org/2000/svg' width='32' height='32'><rect width='32' height='32' rx='6' fill='#777'/></svg>",
    },
  };
}
export function PluginManager() {
  const [rows, setRows] = useState<Summary[]>([]),
    [detail, setDetail] = useState<Detail | null>(null),
    [description, setDescription] = useState(""),
    [proposal, setProposal] = useState(""),
    [confirmed, setConfirmed] = useState(false),
    [body, setBody] = useState("{}"),
    [path, setPath] = useState(""),
    [version, setVersion] = useState(""),
    [secretName, setSecretName] = useState(""),
    [secretValue, setSecretValue] = useState(""),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [result, setResult] = useState("");
  const alive = useRef(false),
    epoch = useRef(0),
    working = useRef(false);
  const reload = useCallback(async () => {
    try {
      const r = await call<Summary[]>("custom.list");
      if (alive.current) setRows(r);
    } catch (e) {
      if (alive.current) setError(message(e));
    }
  }, []);
  useEffect(() => {
    alive.current = true;
    void reload();
    const remove: (() => void)[] = [];
    const clear = () => {
      ++epoch.current;
      setDetail(null);
      setSecretValue("");
      void reload();
    };
    for (const event of ["alfredpet:modules", "alfredpet:settings"])
      void on(event, clear)
        .then((off) => {
          if (alive.current) remove.push(off);
          else off();
        })
        .catch((e) => {
          if (alive.current) setError(message(e));
        });
    return () => {
      alive.current = false;
      ++epoch.current;
      for (const off of remove) off();
    };
  }, [reload]);
  async function perform(action: () => Promise<unknown>) {
    if (working.current) return;
    working.current = true;
    setBusy(true);
    setError("");
    setResult("");
    try {
      const r = await action();
      if (alive.current) {
        setResult(JSON.stringify(r, null, 2));
        setDetail(null);
        await reload();
      }
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      working.current = false;
      if (alive.current) setBusy(false);
    }
  }
  async function inspect(id: string) {
    const seq = ++epoch.current;
    setDetail(null);
    setError("");
    setSecretValue("");
    try {
      const d = await call<Detail>("custom.inspect", { id });
      if (alive.current && seq === epoch.current) {
        setDetail(d);
        setSecretName(d.manifest.secrets?.[0] || "");
      }
    } catch (e) {
      if (alive.current && seq === epoch.current) setError(message(e));
    }
  }
  const expected = detail ? { fingerprint: detail.fingerprint } : null;
  return (
    <section aria-label="Herramientas personalizadas y Plugin Builder">
      <h2>Herramientas personalizadas y Plugin Builder</h2>
      <p>
        Revisa contrato, código, efectos y riesgo antes de generar o probar.
        Cada ejecución necesita aprobación; el proceso conserva acceso a los
        recursos del usuario de Windows.
      </p>
      {error && <p role="alert">{error}</p>}
      <label>
        Describe la herramienta
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
      </label>
      <button
        disabled={busy || !description.trim()}
        onClick={() =>
          void perform(async () => {
            const c = await request<{ id: string }>("conversations.create");
            await request("chat.send", {
              conversation_id: c.id,
              text:
                "Plugin Builder: propone un contrato y código para esta petición: " +
                description +
                "\nDevuelve JSON con manifest y files, sin registrar ni ejecutar código. El usuario debe revisar side effects/risk antes de guardarlo. Sigue el schema de plugin_builder.create: manifest id custom.*, name, version semver X.Y.Z, type custom, runtime node/python/powershell/executable, entrypoint, test, description, inputSchema/outputSchema objeto, permissions {defaultRisk:mandatory,network:boolean,filesystem:[]}, timeoutMs 100..60000, sideEffects[], rollback, secrets[] (sólo nombres ALFREDPET_SECRET_*), fixtures {happy,badInput}. files contiene entrypoint, test, README.md e icon.svg gris. Test recibe ALFREDPET_TEST_CASE=contract para sus assertions y =timeout para una espera de 10s verificable; no incluyas credenciales literales. No actives nada automáticamente.",
            });
            await request("conversations.open", { id: c.id });
            return { conversation_id: c.id, proposal_only: true };
          })
        }
      >
        Pedir propuesta a Claude
      </button>
      <button
        disabled={busy}
        onClick={() => {
          setProposal(JSON.stringify(sampleProposal(), null, 2));
          setConfirmed(false);
        }}
      >
        Cargar ejemplo de eco
      </button>
      <label>
        Propuesta: manifest y files
        <textarea
          value={proposal}
          onChange={(e) => {
            setProposal(e.target.value);
            setConfirmed(false);
          }}
          rows={12}
        />
      </label>
      <label>
        <input
          type="checkbox"
          checked={confirmed}
          onChange={(e) => setConfirmed(e.target.checked)}
        />
        He revisado código, efectos, permisos y rollback; guardaré un borrador
        desactivado.
      </label>
      <button
        disabled={busy || !confirmed || !proposal.trim()}
        onClick={() =>
          void perform(() => {
            const v: Json = JSON.parse(proposal);
            return call("plugin_builder.create", v);
          })
        }
      >
        Revisar y generar borrador
      </button>
      <button disabled={busy} onClick={() => void reload()}>
        Actualizar catálogo
      </button>
      {rows.map((r) => (
        <article className="card" key={r.id}>
          <strong>{r.name}</strong>
          <span>
            {" "}
            · {r.version} · {r.runtime} · {r.state}
          </span>
          <button disabled={busy} onClick={() => void inspect(r.id)}>
            Inspeccionar {r.id}
          </button>
        </article>
      ))}
      {detail && (
        <article className="card">
          <h3>
            {detail.manifest.name} · {detail.manifest.version}
          </h3>
          <p>{detail.manifest.description}</p>
          <p>{detail.isolation}</p>
          <p>Estado: {detail.state}</p>
          {detail.health && <p>Salud: {detail.health}</p>}
          {detail.error_code && <p role="alert">{detail.error_code}</p>}
          <pre>{JSON.stringify(detail.manifest, null, 2)}</pre>
          <pre>{JSON.stringify(detail.tests, null, 2)}</pre>
          <button
            disabled={busy}
            onClick={() =>
              void perform(() =>
                call("plugin_builder.test", { id: detail.id, expected }),
              )
            }
          >
            Revisar y ejecutar seis controles
          </button>
          <button
            disabled={busy || !["tested", "disabled"].includes(detail.state)}
            onClick={() =>
              void perform(() =>
                call("plugin_builder.enable_plugin", {
                  id: detail.id,
                  expected,
                }),
              )
            }
          >
            Revisar y habilitar
          </button>
          <button
            disabled={busy}
            onClick={() =>
              void perform(() =>
                call("custom.configure", {
                  id: detail.id,
                  expected,
                  enabled: false,
                }),
              )
            }
          >
            Desactivar
          </button>
          <label>
            Entrada JSON
            <textarea value={body} onChange={(e) => setBody(e.target.value)} />
          </label>
          <button
            disabled={busy || detail.state !== "enabled"}
            onClick={() =>
              void perform(() =>
                call("custom.run", {
                  id: detail.id,
                  expected,
                  body: JSON.parse(body) as Json,
                }),
              )
            }
          >
            Revisar y ejecutar herramienta
          </button>
          <button
            disabled={busy}
            onClick={() =>
              void perform(() => call("custom.versions", { id: detail.id }))
            }
          >
            Ver versiones conservadas
          </button>
          <label>
            Ruta de nueva versión
            <PathPicker
              label="Carpeta de nueva versión"
              mode="folder"
              value={path}
              onChange={setPath}
              disabled={busy}
            />
          </label>
          <button
            disabled={busy || !path || !proposal}
            onClick={() =>
              void perform(() => {
                const v = JSON.parse(proposal) as { manifest: Json };
                return call("custom.update", {
                  id: detail.id,
                  expected,
                  path,
                  manifest: v.manifest,
                });
              })
            }
          >
            Revisar actualización desde propuesta
          </button>
          <label>
            Versión anterior
            <input
              value={version}
              onChange={(e) => setVersion(e.target.value)}
            />
          </label>
          <button
            disabled={busy || !version}
            onClick={() =>
              void perform(() =>
                call("custom.rollback", { id: detail.id, expected, version }),
              )
            }
          >
            Revisar rollback desactivado
          </button>
          <button
            disabled={busy}
            onClick={() =>
              void perform(() =>
                call("custom.uninstall", { id: detail.id, expected }),
              )
            }
          >
            Revisar retirada del catálogo
          </button>
          {!!detail.manifest.secrets?.length && (
            <>
              <label>
                Referencia privada
                <select
                  value={secretName}
                  onChange={(e) => {
                    setSecretName(e.target.value);
                    setSecretValue("");
                  }}
                >
                  {detail.manifest.secrets.map((s) => (
                    <option key={s}>{s}</option>
                  ))}
                </select>
              </label>
              <label>
                Valor privado
                <input
                  type="password"
                  autoComplete="off"
                  value={secretValue}
                  onChange={(e) => setSecretValue(e.target.value)}
                />
              </label>
              <button
                disabled={busy || !secretValue}
                onClick={() => {
                  const value = secretValue;
                  setSecretValue("");
                  void perform(() =>
                    request("custom.secret_set", {
                      id: detail.id,
                      expected: detail.fingerprint,
                      name: secretName,
                      value,
                    }),
                  );
                }}
              >
                Revisar y guardar en Credential Manager
              </button>
            </>
          )}
        </article>
      )}
      {result && <pre aria-label="Resultado de Plugin Builder">{result}</pre>}
    </section>
  );
}
