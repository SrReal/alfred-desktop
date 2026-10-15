import { useEffect, useState } from "react";
import { on, request } from "../api";
type Rule = { id: string; tool_id: string; scope_json: string };
export function RememberedPermissions() {
  const [rules, setRules] = useState<Rule[]>([]);
  const [error, setError] = useState("");
  const load = () =>
    request<Rule[]>("permissions.rules")
      .then(setRules)
      .catch((e) => setError(String(e)));
  useEffect(() => {
    void load();
    let stop: (() => void) | undefined;
    let active = true;
    void on("alfredpet:permissions_changed", () => {
      if (active) void load();
    })
      .then((fn) => {
        if (active) stop = fn;
        else fn();
      })
      .catch((e) => {
        if (active) setError(String(e));
      });
    return () => {
      active = false;
      stop?.();
    };
  }, []);
  return (
    <section>
      <h2>Permisos recordados</h2>
      <p>
        «Permitir siempre» autoriza únicamente la acción y los parámetros
        revisados. Cambiar el comando, destino o privilegios vuelve a pedir
        permiso. UAC sigue bajo el control de Windows.
      </p>
      <p>
        Revocar impide futuras ejecuciones automáticas; no deshace una acción
        que ya haya comenzado.
      </p>
      {error && <p role="alert">{error}</p>}
      {!rules.length && <p>No hay permisos recordados.</p>}
      {rules.map((rule) => (
        <article className="card" key={rule.id}>
          <strong>{rule.tool_id}</strong>
          <details>
            <summary>Ver alcance exacto</summary>
            <pre>
              {JSON.stringify(JSON.parse(rule.scope_json).preview, null, 2)}
            </pre>
          </details>
          <button
            onClick={() =>
              void request("permissions.revoke", { id: rule.id })
                .then(load)
                .catch((e) => setError(String(e)))
            }
          >
            Revocar permiso
          </button>
        </article>
      ))}
    </section>
  );
}
