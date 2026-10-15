import { useEffect, useRef, useState } from "react";
import { request } from "../api";
import type { Json } from "../types";

type Operation = { method: string; path: string; risk: string };
type Proposal = {
  expected: { fingerprint: string };
  preview: {
    version: string;
    profile: {
      operations: Record<string, Operation>;
      read_only: boolean;
    } & Record<string, Json>;
    diagnostics: { path: string; method: string; code: string }[];
    operations_seen: number;
    operations_proposed: number;
  };
};
export function RestOpenApi({
  connection,
  onApplied,
}: {
  connection: string;
  onApplied: (config: Json) => void;
}) {
  const [remote, setRemote] = useState(true);
  const [document, setDocument] = useState("");
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const epoch = useRef(0);
  const pending = useRef(false);
  useEffect(
    () => () => {
      epoch.current++;
    },
    [],
  );
  function invalidate() {
    epoch.current++;
    setProposal(null);
    setSelected([]);
    setError("");
    setNotice("");
  }
  async function run(apply: boolean) {
    if (pending.current || (apply && (!proposal || !selected.length))) return;
    const ticket = epoch.current;
    pending.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const input: Record<string, Json> = {
        connection,
        action: apply ? "apply" : "preview",
      };
      if (!remote) input.document = JSON.parse(document) as Json;
      if (apply) {
        input.expected = proposal!.expected;
        input.operations = selected;
      }
      const result = await request<Proposal>("tools.execute", {
        tool: "rest.import_openapi",
        input,
      });
      if (ticket !== epoch.current) return;
      if (apply) {
        const profile = proposal!.preview.profile;
        const operations = Object.fromEntries(
          selected.map((id) => [id, profile.operations[id]]),
        );
        onApplied({ ...profile, operations, read_only: true });
        setProposal(null);
        setSelected([]);
        setNotice(
          "Operaciones seleccionadas guardadas. La conexión sigue en solo lectura.",
        );
      } else {
        if (
          !result.expected?.fingerprint ||
          !result.preview?.profile?.operations ||
          !Array.isArray(result.preview.diagnostics)
        )
          throw new Error("La propuesta OpenAPI no es válida.");
        setProposal(result);
        setSelected([]);
      }
    } catch (e) {
      if (ticket === epoch.current) {
        setError(e instanceof Error ? e.message : String(e));
        if (apply) {
          setProposal(null);
          setSelected([]);
        }
      }
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }
  return (
    <div className="card">
      <h2>Importar OpenAPI</h2>
      <p>
        Revisa la propuesta y elige las operaciones que quieres guardar. La
        selección sustituye las operaciones actuales y mantiene solo lectura.
        Los diagnósticos indican operaciones que necesitan una definición
        manual.
      </p>
      <label>
        <input
          type="checkbox"
          checked={remote}
          disabled={busy}
          onChange={(e) => {
            invalidate();
            setRemote(e.target.checked);
          }}
        />
        Consultar openapi_path de esta conexión
      </label>
      {!remote && (
        <textarea
          aria-label="Documento OpenAPI JSON"
          value={document}
          disabled={busy}
          onChange={(e) => {
            invalidate();
            setDocument(e.target.value);
          }}
          spellCheck={false}
          placeholder="Pega el documento OpenAPI en JSON"
        />
      )}
      <button
        disabled={busy || (!remote && !document.trim())}
        onClick={() => void run(false)}
      >
        Revisar OpenAPI
      </button>
      {proposal && (
        <div>
          <p>
            OpenAPI {proposal.preview.version}:{" "}
            {proposal.preview.operations_proposed} de{" "}
            {proposal.preview.operations_seen} operaciones propuestas.
          </p>
          {Object.entries(proposal.preview.profile.operations).map(
            ([id, op]) => (
              <label key={id} className="row">
                <input
                  type="checkbox"
                  aria-label={`Seleccionar ${id}`}
                  disabled={busy}
                  checked={selected.includes(id)}
                  onChange={(e) =>
                    setSelected((old) =>
                      e.target.checked
                        ? [...old, id]
                        : old.filter((v) => v !== id),
                    )
                  }
                />
                <code>
                  {op.method} {op.path}
                </code>
                <span>
                  {id} · {op.risk}
                </span>
              </label>
            ),
          )}
          {proposal.preview.diagnostics.map((d, i) => (
            <p key={i} role="status">
              <code>
                {d.method} {d.path}: {d.code}
              </code>
            </p>
          ))}
          <button
            disabled={busy || !selected.length}
            onClick={() => void run(true)}
          >
            Guardar operaciones seleccionadas
          </button>
        </div>
      )}
      {notice && <p role="status">{notice}</p>}
      {error && <p role="alert">{error}</p>}
    </div>
  );
}
