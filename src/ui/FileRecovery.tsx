import { useCallback, useEffect, useState } from "react";
import { on, request } from "../api";
import type { Json } from "../types";

type Journal = {
  id: string;
  kind: string;
  path: string;
  destination: string | null;
  state: string;
  checkpoint: string | null;
  staging: string | null;
  parked_target: string | null;
  revalidation_available?: boolean;
  failure_reason?: string | null;
};
type Artifact = {
  journal_id: string;
  path: string;
  role: string;
  bytes: number;
  entries: number;
  forgets_recovery: boolean;
  manual_review: string;
};
type Plan = {
  plan_id: string | null;
  artifacts: Artifact[];
  skipped?: { path: string; reason: string }[];
  more?: boolean;
};
type Interrupted = {
  id: string;
  state: string;
  next_index: number;
  error: string | null;
  maintenance?: number;
};
type Maintenance = {
  enabled: boolean;
  staging_days: number | null;
  include_recovery: boolean;
};
type MaintenanceState = {
  status?: string;
  last_error?: string | null;
  scanned?: number;
};
const maintenanceNames: Record<string, string> = {
  disabled: "Desactivado",
  module_disabled: "Módulo de archivos desactivado",
  privacy: "Pausado por privacidad",
  dnd: "Pausado por No molestar",
  age_not_configured: "Configura una antigüedad para comenzar",
  scanned: "Barrido completado; las copias conservadas siguen disponibles",
  awaiting_permission: "Esperando confirmación del lote",
  retained: "Lote conservado; puedes revisarlo manualmente",
  completed: "Lote confirmado y limpiado",
  attention_required: "Requiere revisión",
};
const stateNames: Record<string, string> = {
  available: "Se puede deshacer",
  undone: "Deshecha",
  blocked: "Requiere revisión",
  forgotten: "Recuperación descartada",
  prepared: "Preparada",
  running: "En curso",
  interrupted: "Interrumpida",
  completed: "Completada",
};
async function tool<T>(name: string, input: Json = {}): Promise<T> {
  return request<T>("tools.execute", { tool: `files.${name}`, input });
}
const kindNames: Record<string, string> = {
  archive_batch: "Extracción ZIP combinada",
  tree_copy: "Copia",
  tree_move: "Movimiento",
  tree_cross_move: "Movimiento entre volúmenes",
  tree_delete: "Borrado",
  write: "Escritura",
  copy: "Copia",
  move: "Movimiento",
  delete: "Borrado",
};
const roleNames: Record<string, string> = {
  checkpoint: "Copia de recuperación",
  parked_target: "Destino conservado",
  staging: "Copia incompleta",
};
export function FileRecovery() {
  const [rows, setRows] = useState<Journal[]>([]);
  const [plans, setPlans] = useState<Interrupted[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [forget, setForget] = useState(false);
  const [changed, setChanged] = useState(false);
  const [legacy, setLegacy] = useState(false);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [days, setDays] = useState("");
  const [configured, setConfigured] = useState(false);
  const [automatic, setAutomatic] = useState(false);
  const [stageDays, setStageDays] = useState("");
  const [includeRecovery, setIncludeRecovery] = useState(false);
  const [maintenance, setMaintenance] = useState<MaintenanceState>({});
  const reload = useCallback(async () => {
    const [journals, pending, settings] = await Promise.all([
      tool<Journal[]>("undo_history"),
      tool<Interrupted[]>("cleanup_plans"),
      request<{ key: string; json_value: string }[]>("settings.get"),
    ]);
    const days = JSON.parse(
      settings.find((row) => row.key === "file_checkpoint_days")?.json_value ??
        "null",
    ) as number | null;
    setDays(days === null ? "" : String(days));
    setConfigured(days !== null);
    const config = JSON.parse(
      settings.find((row) => row.key === "file_maintenance")?.json_value ??
        '{"enabled":false,"staging_days":null,"include_recovery":false}',
    ) as Maintenance;
    setAutomatic(config.enabled);
    setStageDays(
      config.staging_days === null ? "" : String(config.staging_days),
    );
    setIncludeRecovery(config.include_recovery);
    setMaintenance(
      JSON.parse(
        settings.find((row) => row.key === "file_maintenance_state")
          ?.json_value ?? "{}",
      ) as MaintenanceState,
    );
    setRows(journals);
    setPlans(
      pending.filter((p) =>
        ["prepared", "interrupted", "failed"].includes(p.state),
      ),
    );
  }, []);
  useEffect(() => {
    void reload().catch((e) => setError(String(e)));
    let disposed = false;
    let stop: (() => void) | undefined;
    void on<MaintenanceState>("alfredpet:file_maintenance", (state) => {
      if (disposed) return;
      setMaintenance(state);
      void Promise.all([
        tool<Journal[]>("undo_history"),
        tool<Interrupted[]>("cleanup_plans"),
      ])
        .then(([journals, pending]) => {
          if (disposed) return;
          setRows(journals);
          setPlans(
            pending.filter((p) =>
              ["prepared", "interrupted", "failed"].includes(p.state),
            ),
          );
        })
        .catch((e) => {
          if (!disposed) setError(String(e));
        });
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch((e) => {
        if (!disposed) setError(String(e));
      });
    return () => {
      disposed = true;
      stop?.();
    };
  }, [reload]);
  async function action(work: () => Promise<void>) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      void reload().catch((e) => setError(String(e)));
    }
  }
  function invalidate() {
    setPlan(null);
    setNotice("");
  }
  return (
    <section aria-label="Recuperación de archivos">
      <h1>Recuperación de archivos</h1>
      <p>
        Las copias de recuperación se conservan hasta que revises y solicites su
        borrado. Por defecto se ofrecen sólo las de operaciones ya deshechas.
      </p>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <button
        disabled={busy}
        onClick={() =>
          void action(async () => {
            invalidate();
            await reload();
          })
        }
      >
        Actualizar recuperación
      </button>
      <fieldset disabled={busy}>
        <legend>Retención por antigüedad</legend>
        <p>
          Deja los días vacíos para seleccionar las copias manualmente. La
          revisión por antigüedad incluye sólo operaciones deshechas; después se
          pide confirmación para borrar.
        </p>
        <label>
          Días de retención
          <input
            aria-label="Días de retención"
            type="number"
            min="1"
            max="3650"
            value={days}
            onChange={(e) => {
              invalidate();
              setDays(e.target.value);
              setConfigured(false);
            }}
          />
        </label>
        <button
          onClick={() =>
            void action(async () => {
              const value = days.trim() === "" ? null : Number(days);
              if (
                value !== null &&
                (!Number.isInteger(value) || value < 1 || value > 3650)
              )
                throw new Error("La retención debe estar entre 1 y 3650 días.");
              await request("settings.set", {
                key: "file_checkpoint_days",
                value,
              });
              invalidate();
              setNotice(
                value === null
                  ? "Selección manual guardada."
                  : "Retención guardada para preparar revisiones.",
              );
            })
          }
        >
          Guardar retención
        </button>
        <button
          disabled={!configured}
          onClick={() =>
            void action(async () => {
              setPlan(await tool<Plan>("retention_preview"));
            })
          }
        >
          Preparar revisión por antigüedad
        </button>
      </fieldset>
      <fieldset disabled={busy}>
        <legend>Mantenimiento periódico</legend>
        <p>
          Al activarlo, se prepara un lote de hasta cien operaciones cada
          minuto. Cada borrado solicita confirmación L3. Los lotes rechazados o
          sin respuesta se conservan y no se vuelven a solicitar mientras sigan
          iguales.
        </p>
        <label className="toggle">
          <input
            type="checkbox"
            checked={automatic}
            onChange={(e) => setAutomatic(e.target.checked)}
          />
          Preparar lotes automáticamente
        </label>
        <label>
          Días para revisar copias incompletas
          <input
            aria-label="Días para revisar copias incompletas"
            type="number"
            min="1"
            max="3650"
            value={stageDays}
            onChange={(e) => setStageDays(e.target.value)}
          />
        </label>
        <p>
          Vacío conserva las copias incompletas. Limpiarlas mantiene las copias
          útiles para deshacer.
        </p>
        <label className="toggle">
          <input
            type="checkbox"
            checked={includeRecovery}
            onChange={(e) => setIncludeRecovery(e.target.checked)}
          />
          Proponer también copias de recuperaciones disponibles o bloqueadas
        </label>
        <p>
          Confirmar su borrado pierde esa recuperación. Las identidades
          desconocidas y las copias modificadas requieren revisión manual.
        </p>
        <button
          onClick={() =>
            void action(async () => {
              const value = stageDays.trim() === "" ? null : Number(stageDays);
              if (
                value !== null &&
                (!Number.isInteger(value) || value < 1 || value > 3650)
              )
                throw new Error(
                  "La antigüedad debe estar entre 1 y 3650 días.",
                );
              await request("settings.set", {
                key: "file_maintenance",
                value: {
                  enabled: automatic,
                  staging_days: value,
                  include_recovery: includeRecovery,
                },
              });
              invalidate();
              setNotice(
                "Mantenimiento guardado. Cambiar la configuración cancela el lote automático pendiente.",
              );
            })
          }
        >
          Guardar mantenimiento
        </button>
        <p aria-label="Estado del mantenimiento">
          {maintenanceNames[maintenance.status ?? "disabled"] ??
            maintenance.status}
        </p>
        {maintenance.last_error && (
          <p>Último resultado: {maintenance.last_error}</p>
        )}
      </fieldset>
      <fieldset disabled={busy}>
        <legend>Operaciones recientes</legend>
        {!rows.length && <p>No hay operaciones registradas.</p>}
        {rows.map((row) => (
          <article className="card" key={row.id}>
            <label>
              <input
                type="checkbox"
                aria-label={`Seleccionar ${row.path}${row.destination ? ` → ${row.destination}` : ""}`}
                checked={selected.includes(row.id)}
                onChange={(e) => {
                  invalidate();
                  setSelected(
                    e.target.checked
                      ? [...selected, row.id]
                      : selected.filter((id) => id !== row.id),
                  );
                }}
              />
              {row.path}
            </label>
            {row.destination && <p>Destino: {row.destination}</p>}
            <p>
              {stateNames[row.state] ?? row.state} ·{" "}
              {kindNames[row.kind] ?? row.kind}
            </p>
            {row.failure_reason && (
              <p>
                Último fallo: {row.failure_reason}. Se comprobarán los cambios
                posteriores antes de deshacer.
              </p>
            )}
            {[row.checkpoint, row.staging, row.parked_target]
              .filter(Boolean)
              .map((path) => (
                <p key={path}>{path}</p>
              ))}
            {(row.state === "available" || row.revalidation_available) && (
              <button
                onClick={() =>
                  void action(async () => {
                    const result = await tool<{ reconciled?: boolean }>(
                      "undo",
                      {
                        id: row.id,
                        ...(row.state === "blocked"
                          ? { revalidate: true }
                          : {}),
                      },
                    );
                    invalidate();
                    setNotice(
                      result.reconciled
                        ? "Estado de recuperación verificado."
                        : "Operación deshecha.",
                    );
                  })
                }
              >
                {row.state === "blocked"
                  ? "Revalidar y solicitar deshacer"
                  : "Solicitar deshacer"}
              </button>
            )}
          </article>
        ))}
      </fieldset>
      <fieldset disabled={busy}>
        <legend>Opciones de la revisión</legend>
        <label className="toggle">
          <input
            type="checkbox"
            checked={forget}
            onChange={(e) => {
              invalidate();
              setForget(e.target.checked);
            }}
          />
          Permitir descartar una recuperación aún disponible o bloqueada
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={changed}
            onChange={(e) => {
              invalidate();
              setChanged(e.target.checked);
            }}
          />
          Incluir artefactos que cambiaron, revisando su contenido actual
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={legacy}
            onChange={(e) => {
              invalidate();
              setLegacy(e.target.checked);
            }}
          />
          Revisar artefactos antiguos cuya identidad no estaba registrada
        </label>
      </fieldset>
      <button
        disabled={busy || !selected.length}
        onClick={() =>
          void action(async () => {
            setPlan(
              await tool<Plan>("cleanup", {
                body: {
                  action: "preview",
                  ids: selected,
                  forget_undo: forget,
                  include_changed: changed,
                  adopt_legacy: legacy,
                },
              }),
            );
          })
        }
      >
        Preparar revisión de limpieza
      </button>
      {plan && (
        <article className="card">
          <h2>Artefactos seleccionados para borrar</h2>
          {plan.artifacts.map((a) => (
            <div key={`${a.journal_id}:${a.path}`}>
              <strong>{a.path}</strong>
              <p>
                {a.bytes.toLocaleString()} bytes · {a.entries} entradas ·{" "}
                {roleNames[a.role] ?? a.role}
              </p>
              {a.forgets_recovery && (
                <p>Se perderá la posibilidad de recuperar esta operación.</p>
              )}
              <p>
                {(
                  {
                    "legacy ownership adopted explicitly":
                      "Identidad antigua revisada expresamente",
                    "changed content adopted explicitly":
                      "Contenido cambiado revisado expresamente",
                    "partial staging": "Copia parcial revisada",
                    "recorded checkpoint": "Identidad y contenido registrados",
                  } as Record<string, string>
                )[a.manual_review] ?? a.manual_review}
              </p>
            </div>
          ))}
          {plan.skipped?.map((s) => (
            <p key={s.path}>
              Conservado: {s.path} · {s.reason}
            </p>
          ))}
          {!plan.artifacts.length && (
            <p>No hay artefactos que cumplan esta revisión.</p>
          )}
          {plan.more && (
            <p>
              Hay más operaciones antiguas. Vuelve a preparar la revisión por
              antigüedad para recorrer el siguiente lote.
            </p>
          )}
          {plan.plan_id && (
            <>
              <p>
                El borrado es definitivo. La ventana de permisos mostrará este
                plan antes de ejecutarlo.
              </p>
              <button
                disabled={busy}
                onClick={() =>
                  void action(async () => {
                    await tool("cleanup", {
                      body: { action: "apply", plan_id: plan.plan_id },
                    });
                    setPlan(null);
                    setSelected([]);
                    setNotice("Limpieza completada.");
                  })
                }
              >
                Solicitar borrado definitivo
              </button>
            </>
          )}
        </article>
      )}
      {plans.length > 0 && (
        <>
          <h2>Limpiezas pendientes o interrumpidas</h2>
          {plans.map((p) => (
            <article className="card" key={p.id}>
              <p>
                {p.id} · {p.next_index} artefactos completados
              </p>
              {p.error && <p>{p.error}</p>}
              <button
                disabled={busy}
                onClick={() =>
                  void action(async () => {
                    await tool("cleanup", {
                      body: {
                        action: "apply",
                        plan_id: p.id,
                        ...(p.maintenance ? { revalidate_policy: true } : {}),
                      },
                    });
                    setNotice("Limpieza reanudada y completada.");
                  })
                }
              >
                {p.state === "prepared"
                  ? "Revisar limpieza pendiente"
                  : "Revisar y reanudar limpieza"}
              </button>
            </article>
          ))}
        </>
      )}
    </section>
  );
}
