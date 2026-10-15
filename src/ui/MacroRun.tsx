import { useEffect, useRef, useState } from "react";
import { request } from "../api";
import type { Json } from "../types";
import { PathFieldControl, toolPathFields } from "./JsonPathEditor";

type Parameter = {
  type: "string" | "integer" | "number" | "boolean";
  description?: string;
  required?: boolean;
  default?: string | number | boolean;
  enum?: (string | number | boolean)[];
};
type Macro = {
  id: string;
  name: string;
  fingerprint: string;
  enabled: boolean;
  definition: {
    description: string;
    parameters: Record<string, Parameter>;
    error_strategy: "stop" | "continue";
    steps: {
      tool: string;
      input?: Record<string, Json>;
      rollback_hint?: string;
      when?: Json;
    }[];
    shortcut?: string;
  };
};
export function MacroRun({
  id,
  onClose,
  onResult,
  onSavedRoutine,
  initialRoutine,
}: {
  id: string;
  onClose: () => void;
  onResult: (value: Json) => void;
  onSavedRoutine?: () => void;
  initialRoutine?: {
    id: string;
    revision: string;
    name: string;
    body: Record<string, Json>;
  };
}) {
  const [macro, setMacro] = useState<Macro | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [routineName, setRoutineName] = useState("");
  const pending = useRef(false);
  const epoch = useRef(0);
  useEffect(() => {
    const current = ++epoch.current;
    setMacro(null);
    setError("");
    setValues({});
    setBusy(false);
    void request<Macro>("tools.execute", {
      tool: "macros.inspect",
      input: { id },
    })
      .then((result) => {
        if (current !== epoch.current) return;
        if (
          result.id !== id ||
          !result.definition?.parameters ||
          !result.fingerprint
        )
          throw new Error("Respuesta de macro inválida");
        setMacro(result);
        setRoutineName(initialRoutine?.name ?? result.name);
        setValues(
          Object.fromEntries(
            Object.entries(result.definition.parameters)
              .filter(
                ([key, p]) =>
                  initialRoutine?.body[key] !== undefined ||
                  p.default !== undefined,
              )
              .map(([key, p]) => [
                key,
                p.type === "string" && !p.enum
                  ? String(initialRoutine?.body[key] ?? p.default)
                  : JSON.stringify(initialRoutine?.body[key] ?? p.default),
              ]),
          ),
        );
      })
      .catch((e: unknown) => {
        if (current === epoch.current) setError(String(e));
      });
    return () => {
      epoch.current++;
    };
  }, [id, initialRoutine]);
  async function run(saveRoutine = false) {
    if (!macro || pending.current) return;
    pending.current = true;
    const current = epoch.current;
    setBusy(true);
    setError("");
    try {
      const body: Record<string, Json> = {};
      for (const [key, p] of Object.entries(macro.definition.parameters)) {
        const raw = values[key];
        if (raw === undefined || (p.type !== "string" && raw === "")) {
          if (p.required) throw new Error(`Falta el parámetro ${key}`);
          continue;
        }
        let value: Json;
        if (p.enum || p.type === "boolean") value = JSON.parse(raw) as Json;
        else if (p.type === "string") value = raw;
        else {
          const number = Number(raw);
          if (
            !Number.isFinite(number) ||
            (p.type === "integer" && !Number.isSafeInteger(number))
          )
            throw new Error(`Número inválido: ${key}`);
          value = number;
        }
        body[key] = value;
      }
      if (saveRoutine) {
        await request("routines.save", {
          ...(initialRoutine
            ? { id: initialRoutine.id, revision: initialRoutine.revision }
            : {}),
          name: routineName,
          macroId: id,
          fingerprint: macro.fingerprint,
          body,
        });
        if (current === epoch.current) onSavedRoutine?.();
        return;
      }
      const result = await request<Json>("tools.execute", {
        tool: "macros.run",
        input: { id, body, expected: { fingerprint: macro.fingerprint } },
      });
      if (current === epoch.current) onResult(result);
    } catch (e) {
      if (current === epoch.current) setError(String(e));
    } finally {
      pending.current = false;
      if (current === epoch.current) setBusy(false);
    }
  }
  return (
    <section aria-label="Revisar macro">
      <button onClick={onClose} disabled={busy}>
        Cerrar revisión
      </button>
      {!macro && !error && <p>Cargando macro…</p>}
      {error && <p role="alert">{error}</p>}
      {macro && (
        <>
          <h2>{macro.name}</h2>
          <p>{macro.definition.description}</p>
          <p>
            Ante un error:{" "}
            {macro.definition.error_strategy === "stop"
              ? "detener"
              : "continuar"}
            . Los permisos de cada herramienta se revisan por separado.
          </p>
          {Object.entries(macro.definition.parameters).map(([key, p]) => {
            const path = macro.definition.steps.flatMap((step) =>
              toolPathFields(
                step.tool,
                Object.keys(step.input ?? {}),
                step.input,
              ).filter((field) => {
                const value = step.input?.[field.key];
                return (
                  value &&
                  typeof value === "object" &&
                  !Array.isArray(value) &&
                  value.$parameter === key
                );
              }),
            )[0];
            return (
              <label key={key}>
                {key}
                {p.required ? " *" : ""} {p.description}
                {path && p.type === "string" && !p.enum ? (
                  <PathFieldControl
                    field={{
                      ...path,
                      key,
                      label: `Seleccionar ${key}`,
                      multiple: false,
                    }}
                    value={values[key]}
                    disabled={busy}
                    onChange={(value) =>
                      setValues((old) => ({ ...old, [key]: String(value) }))
                    }
                  />
                ) : p.enum || p.type === "boolean" ? (
                  <select
                    value={values[key] ?? ""}
                    onChange={(event) =>
                      setValues((old) => ({
                        ...old,
                        [key]: event.target.value,
                      }))
                    }
                    disabled={busy}
                  >
                    <option value="">Sin valor</option>
                    {(p.enum ?? [true, false]).map((choice) => (
                      <option
                        key={JSON.stringify(choice)}
                        value={JSON.stringify(choice)}
                      >
                        {String(choice)}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    type={p.type === "string" ? "text" : "number"}
                    step={p.type === "integer" ? 1 : "any"}
                    value={values[key] ?? ""}
                    onChange={(event) =>
                      setValues((old) => ({
                        ...old,
                        [key]: event.target.value,
                      }))
                    }
                    disabled={busy}
                  />
                )}
              </label>
            );
          })}
          <ol>
            {macro.definition.steps.map((step, index) => (
              <li key={index}>
                {step.tool}
                {step.when && <span> · condicional</span>}
                {step.input && (
                  <details>
                    <summary>Parámetros del paso</summary>
                    <pre>{JSON.stringify(step.input, null, 2)}</pre>
                  </details>
                )}
                {step.rollback_hint && (
                  <p>
                    Reversión sugerida: {step.rollback_hint}. Se ejecuta sólo si
                    la solicitas.
                  </p>
                )}
              </li>
            ))}
          </ol>
          {onSavedRoutine && (
            <div className="routine-save">
              <label>
                Nombre de rutina
                <input
                  value={routineName}
                  maxLength={200}
                  disabled={busy}
                  onChange={(e) => setRoutineName(e.target.value)}
                />
              </label>
              <p>
                Guarda estos valores para iniciar la macro desde Comandos. No
                guardes contraseñas ni claves. Se mantienen las autorizaciones
                de la macro y de cada paso.
              </p>
              <button
                onClick={() => void run(true)}
                disabled={busy || !macro.enabled || !routineName.trim()}
              >
                {initialRoutine
                  ? "Guardar revisión de rutina"
                  : "Guardar como rutina"}
              </button>
            </div>
          )}
          <button onClick={() => void run()} disabled={busy || !macro.enabled}>
            Revisar y ejecutar macro
          </button>
        </>
      )}
    </section>
  );
}
