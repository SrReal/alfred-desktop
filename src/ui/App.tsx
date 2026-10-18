import { ProfileBackup } from "./ProfileBackup";
import { PathPicker, PathsPicker } from "./PathPicker";
import {
  ConnectionConfig,
  JsonPathEditor,
  jsonObject,
  toolPathFields,
} from "./JsonPathEditor";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { request, show, on, desktop } from "../api";
import { AssetRegistry, gravityFrame, tokens, visualStyle } from "../assets";
import { agentPetState, petStateLabels } from "../petState";
import { usePetAnimation } from "./usePetAnimation";
import { PixelIcon } from "./PixelIcon";
import { ManualHelp } from "./ManualHelp";
import { RememberedPermissions } from "./RememberedPermissions";
import { SystemAudit } from "./SystemAudit";
import { Terminal } from "./Terminal";
import { MacroRun } from "./MacroRun";
import { Routines } from "./Routines";
import { MediaPanel } from "./MediaPanel";
import { DownloadPostActions } from "./DownloadPostActions";
import { PluginManager } from "./PluginManager";
import { PetDrop } from "./PetDrop";
import { ChatFiles } from "./ChatFiles";
import { ChatExplain, explainError } from "./ChatExplain";
import { ChatDictation } from "./ChatDictation";
import { ChatCapabilities } from "./ChatCapabilities";
import { useChatProgress, runStatus } from "./ChatProgress";
import { ChatRevision, type Revision } from "./ChatRevision";
import { ChatTranscript } from "./ChatTranscript";
import {
  ClaudeModelSelect,
  claudeModels,
  configuredClaudeModel,
} from "./ClaudeModel";
import { TasksPanel } from "./TasksPanel";
import { ClipboardPanel } from "./ClipboardPanel";
import { DriveAuthorization } from "./DriveAuthorization";
import { RestOpenApi } from "./RestOpenApi";
import { McpInspector } from "./McpInspector";
import { MemoryAnalysis } from "./MemoryAnalysis";
import { SemanticMemory } from "./SemanticMemory";
import { FileRecovery } from "./FileRecovery";
import { WindowRecovery } from "./WindowRecovery";
import {
  AssetSettings,
  CaptureAsset,
  ConversationAttachments,
} from "./ManagedAssets";
import type {
  Activity,
  AgentEvent,
  Conversation,
  Json,
  Memory,
  Message,
  Module,
  Permission,
  PetState,
  Project,
  Tool,
  Watch,
} from "../types";
const surfaces = [
  ["chat", "Chat"],
  ["history", "Historial"],
  ["command_palette", "Comandos"],
  ["task_center", "Tareas"],
  ["activity", "Actividad"],
  ["settings", "Configuración"],
] as const;
function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
function useData<T>(
  operation: string,
  input: Json = {},
  event?: string,
  enabled = true,
) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState("");
  const key = JSON.stringify(input);
  const generation = useRef(0);
  const identity = useRef("");
  const currentIdentity = operation + key + enabled;
  if (identity.current !== currentIdentity) {
    identity.current = currentIdentity;
    generation.current++;
  }
  const reload = useCallback(async () => {
    if (!enabled || identity.current !== operation + key + enabled) return;
    const current = ++generation.current;
    try {
      const next = await request<T>(operation, JSON.parse(key) as Json);
      if (current !== generation.current) return;
      setData(next);
      setError("");
    } catch (e) {
      if (current === generation.current) setError(errorText(e));
    }
  }, [operation, key, enabled]);
  useEffect(() => {
    if (!enabled) return;
    void reload();
    let alive = true;
    let unlisten: (() => void) | undefined;
    if (event)
      void on(event, () => void reload()).then((f) => {
        if (alive) unlisten = f;
        else f();
      });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [reload, event, enabled]);
  return { data, error, reload };
}
function ErrorNotice({ text }: { text: string }) {
  return text ? (
    <div className="error" role="alert">
      {text}
    </div>
  ) : null;
}
function Empty({ children }: { children: React.ReactNode }) {
  return <div className="empty">{children}</div>;
}
export function App() {
  const [surface, setSurface] = useState(
    () => new URLSearchParams(location.search).get("surface") ?? "chat",
  );
  const [visited, setVisited] = useState(() => new Set([surface]));
  const [error, setError] = useState("");
  function navigate(next: string) {
    const url = new URL(location.href);
    url.searchParams.set("surface", next);
    history.replaceState({}, "", url);
    setVisited((previous) => new Set([...previous, next]));
    setSurface(next);
  }
  useEffect(() => {
    if (
      !surfaces.some(
        ([id]) => id === new URLSearchParams(location.search).get("surface"),
      )
    )
      return;
    let live = true;
    let off: (() => void) | undefined;
    void on<{ surface: string }>("alfredpet:navigate", (e) => {
      if (surfaces.some(([id]) => id === e.surface)) navigate(e.surface);
    }).then((f) => {
      if (live) off = f;
      else f();
    });
    return () => {
      live = false;
      off?.();
    };
  }, []);
  useEffect(() => {
    if (!desktop) return;
    const timer = setTimeout(() => {
      void request<{ surface?: string }>("window.ready", { surface })
        .then((r) => {
          if (r.surface && r.surface !== surface) navigate(r.surface);
        })
        .catch((e) => setError(errorText(e)));
    }, 0);
    return () => clearTimeout(timer);
  }, [surface]);
  if (surface === "pet") return <Pet />;
  if (surface === "bubble") return <Bubble />;
  if (surface === "capture_overlay") return <CaptureOverlay />;
  const content =
    surface === "permission" ? (
      <Permissions />
    ) : (
      <Empty>Ventana desconocida</Empty>
    );
  return (
    <div
      className={`app surface-${surface}`}
      style={visualStyle as CSSProperties}
      onDragOver={(e) => {
        if (Array.from(e.dataTransfer.types).includes("Files"))
          e.preventDefault();
      }}
      onDrop={(e) => {
        if (!Array.from(e.dataTransfer.types).includes("Files")) return;
        e.preventDefault();
        if (surface !== "chat")
          setError("Abre Chat para adjuntar los archivos.");
      }}
    >
      <header>
        <div>
          <span className="brand-mark" aria-hidden="true">
            A
          </span>
          <strong>AlfredPet</strong>
          <span className="badge">
            {surfaces.find((s) => s[0] === surface)?.[1] ??
              (surface === "permission" ? "Permisos" : "Aviso")}
          </span>
        </div>
        <nav aria-label="Ventanas de AlfredPet">
          {surfaces
            .filter((s) => s[0] !== surface)
            .map(([id, label]) => (
              <button
                key={id}
                onClick={() =>
                  desktop
                    ? void show(id).catch((e) => setError(errorText(e)))
                    : navigate(id)
                }
              >
                <SurfaceIcon surface={id} />
                <span>{label}</span>
              </button>
            ))}
        </nav>
      </header>
      {!desktop && (
        <div className="preview">
          Vista previa de interfaz · sin conexión a herramientas ni Claude
        </div>
      )}
      <ErrorNotice text={error} />
      <main>
        {surfaces.some(([id]) => id === surface)
          ? surfaces
              .filter(([id]) => visited.has(id))
              .map(([id]) => (
                <div
                  key={id}
                  style={{ display: id === surface ? "contents" : "none" }}
                >
                  {id === "chat" ? (
                    <Chat active={id === surface} />
                  ) : id === "history" ? (
                    <History />
                  ) : id === "settings" ? (
                    <Settings />
                  ) : id === "activity" ? (
                    <ActivityCenter />
                  ) : id === "task_center" ? (
                    <Tasks />
                  ) : (
                    <Palette />
                  )}
                </div>
              ))
          : content}
      </main>
    </div>
  );
}
function SurfaceIcon({ surface }: { surface: string }) {
  const name =
    (
      { command_palette: "commands", task_center: "tasks" } as Record<
        string,
        string
      >
    )[surface] ?? surface;
  return <PixelIcon name={name} />;
}

function Pet() {
  const [state, setState] = useState<PetState>("idle");
  const [error, setError] = useState("");
  const settings = useData<{ key: string; json_value: string }[]>(
    "settings.get",
    {},
    "alfredpet:settings",
  );
  const privacy = settings.data?.some(
    (item) => item.key === "privacy" && item.json_value === "true",
  );
  const dnd = settings.data?.some(
    (item) => item.key === "dnd" && item.json_value === "true",
  );
  const visibleState = privacy ? "privacy" : dnd ? "dnd" : state;
  const spriteStyle = usePetAnimation(visibleState, setError);
  useEffect(() => {
    if (state !== "success" && state !== "error") return;
    const delay = AssetRegistry.pet[state].durations.reduce(
      (sum, ms) => sum + ms,
      0,
    );
    const timer = setTimeout(
      () => setState((current) => (current === state ? "idle" : current)),
      delay,
    );
    return () => clearTimeout(timer);
  }, [state]);
  const drag = useRef<{ x: number; y: number } | null>(null);
  const moving = useRef(false);
  const dragged = useRef(false);
  const generation = useRef(0);
  const fall = useCallback(async (restore = false) => {
    if (!desktop) return;
    const gen = ++generation.current;
    setState("falling");
    const p = await request<{ x: number; y: number; from_y: number }>(
      restore ? "pet.restore" : "pet.settle",
    );
    const start = performance.now();
    await new Promise<void>((resolve, reject) => {
      const tick = async () => {
        if (gen !== generation.current) {
          resolve();
          return;
        }
        const elapsed = performance.now() - start;
        const y =
          elapsed >= tokens.maxFallMs
            ? p.y
            : gravityFrame(p.from_y, p.y, elapsed);
        await getCurrentWindow().setPosition(
          new PhysicalPosition(Math.round(p.x), Math.round(y)),
        );
        if (y >= p.y) {
          resolve();
          setState("idle");
        } else requestAnimationFrame(() => void tick().catch(reject));
      };
      void tick().catch(reject);
    });
  }, []);
  useEffect(() => {
    void fall(true).catch((e) => setError(errorText(e)));
    let live = true;
    const unlisteners: (() => void)[] = [];
    for (const [name, handler] of [
      [
        "alfredpet:agent",
        (e: AgentEvent) => {
          setState(agentPetState(e));
        },
      ],
      ["alfredpet:permission", () => setState("waiting_permission")],
      ["alfredpet:question", () => setState("waiting_user")],
      ["alfredpet:pet_settle", () => void fall()],
    ] as const) {
      void on<AgentEvent>(name, handler).then((f) => {
        if (live) unlisteners.push(f);
        else f();
      });
    }
    return () => {
      live = false;
      unlisteners.forEach((f) => f());
      generation.current++;
    };
  }, [fall]);
  async function move(e: React.PointerEvent) {
    if (!drag.current || moving.current) return;
    if (Math.hypot(e.screenX - drag.current.x, e.screenY - drag.current.y) < 5)
      return;
    moving.current = true;
    dragged.current = true;
    generation.current++;
    setState("dragging");
    try {
      await request("pet.drag");
      await fall();
    } catch (err) {
      setError(errorText(err));
    } finally {
      moving.current = false;
      drag.current = null;
    }
  }
  return (
    <button
      className="pet"
      aria-label="AlfredPet mascota"
      title={error || `AlfredPet · ${petStateLabels[visibleState]}`}
      data-state={visibleState}
      style={spriteStyle}
      onPointerEnter={() =>
        setState((current) => (current === "idle" ? "hover" : current))
      }
      onPointerLeave={() =>
        setState((current) => (current === "hover" ? "idle" : current))
      }
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        dragged.current = false;
        drag.current = { x: e.screenX, y: e.screenY };
      }}
      onPointerMove={(e) => void move(e)}
      onPointerUp={() => {
        drag.current = null;
      }}
      onPointerCancel={() => {
        drag.current = null;
        dragged.current = true;
      }}
      onClick={(e) => {
        if (
          e.button !== 0 ||
          e.detail > 1 ||
          (e.detail > 0 && (moving.current || dragged.current))
        )
          return;
        void request("window.toggle", { surface: "chat" }).catch((e) =>
          setError(errorText(e)),
        );
      }}
      onDoubleClick={() =>
        void show("command_palette").catch((e) => setError(errorText(e)))
      }
      onContextMenu={(e) => {
        e.preventDefault();
        void request("pet.menu").catch((e) => setError(errorText(e)));
      }}
    >
      <span className="sr-only">{petStateLabels[visibleState]}</span>
    </button>
  );
}
function Chat({ active }: { active: boolean }) {
  const modelSettings = useData<{ key: string; json_value: string }[]>(
    "settings.get",
    {},
    "alfredpet:settings",
  );
  const [modelBusy, setModelBusy] = useState(false);
  const model = configuredClaudeModel(modelSettings.data);
  const [sending, setSending] = useState(false);
  const conversations = useData<Conversation[]>(
    "conversations.list",
    {},
    "alfredpet:conversations",
  );
  const [selected, setSelected] = useState("");
  const progress = useChatProgress(selected);
  const activeProgress = progress.runs.find((r) => r.status === "running");
  const [draft, setDraft] = useState("");
  const [stream, setStream] = useState("");
  const [replies, setReplies] = useState<Message[]>([]);
  const [deleting, setDeleting] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Conversation | null>(null);
  const finishedRuns = useRef(new Set<string>());
  const [run, setRun] = useState("");
  const [error, setError] = useState("");
  const [attaching, setAttaching] = useState(false);
  const [dictating, setDictating] = useState(false);
  const [revision, setRevision] = useState<Revision | null>(null);
  const draftVersion = useRef(0);
  const messages = useData<Message[]>("conversations.messages", {
    id: selected,
  });
  const reloadMessages = useRef(messages.reload);
  reloadMessages.current = messages.reload;
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  useEffect(() => {
    setReplies([]);
    setStream("");
    setRun("");
    setDeleteTarget(null);
    setRevision(null);
    let live = true;
    const expectedDraft = draftVersion.current;
    if (selected)
      void request<string | null>("chat.draft", { id: selected })
        .then((text) => {
          if (
            live &&
            expectedDraft === draftVersion.current &&
            typeof text === "string"
          )
            setDraft((previous) => previous || text);
        })
        .catch((e) => {
          if (live) setError(errorText(e));
        });
    const completedBefore = finishedRuns.current.size;
    if (selected)
      void request<{ running: boolean }>("chat.state", { id: selected })
        .then((state) => {
          if (
            live &&
            state.running &&
            completedBefore === finishedRuns.current.size
          )
            setRun(selected);
        })
        .catch((e) => {
          if (live) setError(errorText(e));
        });
    return () => {
      live = false;
    };
  }, [selected]);
  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    void request<string | null>("conversations.active")
      .then((id) => {
        if (live && typeof id === "string") setSelected(id);
      })
      .catch(() => {});
    void on<{ id: string }>("alfredpet:conversation_selected", (e) => {
      setSelected(e.id);
      setStream("");
      setRun("");
    }).then((f) => {
      if (live) off = f;
      else f();
    });
    return () => {
      live = false;
      off?.();
    };
  }, []);
  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    void on<AgentEvent>("alfredpet:agent", (e) => {
      if (e.type === "completed" || e.type === "error")
        finishedRuns.current.add(e.run_id);
      if (e.conversation_id !== selectedRef.current) return;
      if (e.type === "response" && e.message) {
        const message = e.message;
        setRun(e.run_id);
        setReplies((rows) =>
          rows.some((r) => r.id === message.id)
            ? rows.map((r) => (r.id === message.id ? message : r))
            : [...rows, message],
        );
      }
      if (e.type === "text_delta") setStream((s) => s + (e.text ?? ""));
      else if (e.type === "tool_requested") {
        setRun(e.run_id);
      } else if (e.type === "completed" || e.type === "error") {
        setRun("");
        setStream("");
        if (e.error) setError(e.error);
        setReplies((rows) =>
          rows.map((r) => ({
            ...r,
            status:
              r.status === "complete"
                ? "complete"
                : e.type === "error"
                  ? "interrupted"
                  : "complete",
          })),
        );
        void messages.reload();
        void conversations.reload();
      }
    }).then((f) => {
      if (live) off = f;
      else f();
    });
    return () => {
      live = false;
      off?.();
    };
  }, [messages.reload, conversations.reload]);
  useEffect(() => {
    if (!deleting && !selected && conversations.data?.[0])
      setSelected(conversations.data[0].id);
  }, [conversations.data, selected, deleting]);
  async function create() {
    const row = await request<{ id: string }>("conversations.create");
    setSelected(row.id);
    setStream("");
    await conversations.reload();
    return row.id;
  }
  async function deleteChat() {
    if (!deleteTarget || deleting || run || sending || attaching) return;
    setDeleting(true);
    setError("");
    try {
      await request("conversations.delete", {
        id: deleteTarget.id,
        updated_at: deleteTarget.updated_at,
        confirmed: true,
      });
      setSelected("");
      setReplies([]);
      setStream("");
      setDraft("");
      setDeleteTarget(null);
      await conversations.reload();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setDeleting(false);
    }
  }
  async function selectModel(value: string) {
    setModelBusy(true);
    setError("");
    try {
      await request("settings.set", { key: "claude_model", value });
      await modelSettings.reload();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setModelBusy(false);
    }
  }
  async function send() {
    if (
      !draft.trim() ||
      revision ||
      dictating ||
      deleting ||
      deleteTarget ||
      run ||
      sending ||
      attaching ||
      modelBusy ||
      !modelSettings.data ||
      modelSettings.error
    )
      return;
    draftVersion.current++;
    setSending(true);
    setError("");
    try {
      const id = selected || (await create());
      setStream("");
      setReplies([]);
      const result = await request<{ run_id: string }>("chat.send", {
        conversation_id: id,
        text: draft,
      });
      draftVersion.current++;
      setDraft("");
      if (!finishedRuns.current.has(result.run_id)) setRun(result.run_id);
      await reloadMessages.current();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSending(false);
    }
  }
  async function submitRevision() {
    if (
      !revision ||
      sending ||
      run ||
      dictating ||
      attaching ||
      deleting ||
      modelBusy ||
      draft.trim()
    )
      return;
    draftVersion.current++;
    setSending(true);
    setError("");
    try {
      const branch = await request<{ id: string; text: string }>(
        "conversations.branch",
        {
          conversation_id: selected,
          message_id: revision.message.id,
          expected_text: revision.message.content_text,
          mode: revision.mode,
          text: revision.text,
        },
      );
      selectedRef.current = branch.id;
      setSelected(branch.id);
      setRevision(null);
      setDraft(branch.text);
      setReplies([]);
      setStream("");
      await conversations.reload();
      const result = await request<{ run_id: string }>("chat.send", {
        conversation_id: branch.id,
        text: branch.text,
      });
      draftVersion.current++;
      setDraft("");
      if (!finishedRuns.current.has(result.run_id)) setRun(result.run_id);
      await reloadMessages.current();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSending(false);
    }
  }
  const revisionBusy =
    !!run ||
    sending ||
    attaching ||
    deleting ||
    dictating ||
    modelBusy ||
    !!deleteTarget ||
    !modelSettings.data ||
    !!modelSettings.error;
  const parent = conversations.data?.find(
    (c) => c.id === selected,
  )?.parent_conversation_id;
  return (
    <ChatFiles
      busy={!!run || sending || deleting || dictating || !!revision}
      conversation={() => Promise.resolve(selected || create())}
      onBusy={setAttaching}
      onError={setError}
      actions={
        !revision && (
          <div className="chat-context-actions">
            <ChatCapabilities
              busy={
                !!run ||
                sending ||
                deleting ||
                dictating ||
                !!revision ||
                !!draft.trim()
              }
              onDraft={setDraft}
            />
            <ChatExplain
              active={active}
              busy={
                !!run ||
                sending ||
                deleting ||
                attaching ||
                dictating ||
                !!deleteTarget ||
                !!revision
              }
              conversation={() => Promise.resolve(selected || create())}
              onBusy={setAttaching}
              onDraft={(text) =>
                setDraft((previous) =>
                  [previous.trim(), text].filter(Boolean).join("\n\n"),
                )
              }
            />
            <ChatDictation
              active={active}
              conversationId={selected}
              busy={
                !!run ||
                sending ||
                deleting ||
                attaching ||
                !!deleteTarget ||
                !!revision
              }
              onBusy={setDictating}
              onDraft={(text) =>
                setDraft((previous) =>
                  [previous, text].filter(Boolean).join("\n\n"),
                )
              }
            />
          </div>
        )
      }
    >
      <section className="chat">
        <PetDrop
          conversations={conversations.data ?? []}
          busy={!!run || sending || !!revision}
          onAttached={(id, analysis) => {
            setSelected(id);
            if (analysis)
              setDraft((previous) =>
                [
                  previous.trim(),
                  "Analiza los archivos adjuntos de esta conversación.",
                ]
                  .filter(Boolean)
                  .join("\n\n"),
              );
            void conversations.reload();
          }}
        />
        <div className="toolbar">
          <select
            aria-label="Conversación"
            disabled={
              sending || deleting || attaching || dictating || !!revision
            }
            value={selected}
            onChange={(e) => {
              setSelected(e.target.value);
              setStream("");
            }}
          >
            <option value="">Nueva conversación</option>
            {conversations.data?.map((c) => (
              <option value={c.id} key={c.id}>
                {c.title}
              </option>
            ))}
          </select>
          <ClaudeModelSelect
            value={model}
            disabled={
              !!run ||
              sending ||
              modelBusy ||
              !modelSettings.data ||
              !!modelSettings.error
            }
            onChange={(value) => void selectModel(value)}
          />
          <button
            className="icon-action"
            aria-label="Nueva"
            disabled={
              sending || deleting || attaching || dictating || !!revision
            }
            title="Nueva conversación"
            onClick={() => void create().catch((e) => setError(errorText(e)))}
          >
            <PixelIcon name="add" />
          </button>
          <button
            className="icon-action"
            aria-label="Desde aquí"
            title="Crear conversación desde aquí"
            disabled={
              !selected ||
              sending ||
              deleting ||
              attaching ||
              dictating ||
              !!revision
            }
            onClick={() =>
              void request<{ id: string }>("conversations.fork", {
                id: selected,
              })
                .then((r) => {
                  setSelected(r.id);
                  void conversations.reload();
                })
                .catch((e) => setError(errorText(e)))
            }
          >
            <PixelIcon name="next" />
          </button>
          <button
            className="icon-action"
            aria-label="Borrar chat"
            title="Borrar conversación"
            disabled={
              !conversations.data?.some((c) => c.id === selected) ||
              !!run ||
              sending ||
              attaching ||
              deleting ||
              dictating ||
              !!revision
            }
            onClick={() =>
              setDeleteTarget(
                conversations.data?.find((c) => c.id === selected) ?? null,
              )
            }
          >
            <PixelIcon name="remove" />
          </button>
        </div>
        {deleteTarget && (
          <div
            className="chat-delete-confirm"
            role="alertdialog"
            aria-label="Confirmar borrado de chat"
            aria-describedby="chat-delete-description"
          >
            <strong>Borrar «{deleteTarget.title}»</strong>
            <p id="chat-delete-description">
              Se eliminarán esta conversación local, sus mensajes y los vínculos
              a adjuntos. Se conservan los archivos originales y los recuerdos
              guardados por separado. No se puede deshacer.
            </p>
            <div className="row">
              <button disabled={deleting} onClick={() => setDeleteTarget(null)}>
                Cancelar
              </button>
              <button
                className="danger"
                disabled={deleting}
                onClick={() => void deleteChat()}
              >
                {deleting ? "Borrando…" : "Borrar definitivamente"}
              </button>
            </div>
          </div>
        )}
        <ErrorNotice
          text={
            error ||
            messages.error ||
            conversations.error ||
            modelSettings.error
          }
        />
        <ErrorNotice text={progress.error} />
        {parent && (
          <div className="chat-branch-origin">
            Variante ·{" "}
            <button
              disabled={sending || !!revision || !!draft.trim()}
              onClick={() => setSelected(parent)}
            >
              Volver al original
            </button>
          </div>
        )}
        <ChatTranscript
          actionsDisabled={revisionBusy || !!revision || !!draft.trim()}
          onRevise={setRevision}
          progress={progress.runs}
          conversationId={selected}
          messages={messages.data}
          stream={stream}
          replies={replies}
          pendingText={
            run || sending
              ? activeProgress
                ? runStatus(activeProgress)
                : "Alfred está trabajando"
              : ""
          }
        />
        {revision && (
          <ChatRevision
            value={revision}
            busy={revisionBusy}
            onCancel={() => setRevision(null)}
            onChange={(text) => setRevision((r) => (r ? { ...r, text } : r))}
            onSubmit={() => void submitRevision()}
          />
        )}
        {!revision && (
          <>
            <ConversationAttachments
              conversationId={selected}
              busy={!!run || attaching || dictating || sending || !!revision}
            />
            <div className="composer">
              <textarea
                aria-label="Mensaje"
                disabled={deleting || sending || !!revision}
                value={draft}
                onChange={(e) => {
                  draftVersion.current++;
                  setDraft(e.target.value);
                }}
                placeholder="Escribe a AlfredPet…"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    void send();
                  }
                }}
              />
              {run ? (
                <button
                  onClick={() =>
                    void request("chat.interrupt", { run_id: run }).catch((e) =>
                      setError(errorText(e)),
                    )
                  }
                >
                  <PixelIcon name="stop" /> Interrumpir
                </button>
              ) : (
                <button
                  className="primary"
                  disabled={
                    !draft.trim() ||
                    !!revision ||
                    dictating ||
                    deleting ||
                    !!deleteTarget ||
                    sending ||
                    attaching ||
                    modelBusy ||
                    !modelSettings.data ||
                    !!modelSettings.error
                  }
                  onClick={() => void send()}
                >
                  <PixelIcon name="send" /> Enviar
                </button>
              )}
            </div>
          </>
        )}
      </section>
    </ChatFiles>
  );
}
function History() {
  const rows = useData<Conversation[]>(
    "conversations.list",
    {},
    "alfredpet:conversations",
  );
  const [query, setQuery] = useState("");
  const search = useData<
    { id: string; kind: string; title: string; snippet: string }[]
  >("search", { query });
  const [error, setError] = useState("");
  return (
    <section>
      <h1>Historial y búsqueda</h1>
      <input
        aria-label="Buscar historial"
        placeholder="Buscar conversaciones, mensajes y memoria"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      <ErrorNotice text={error || rows.error || search.error} />
      {query
        ? search.data?.map((r, i) => (
            <article className="card" key={`${r.id}-${i}`}>
              <span className="badge">{r.kind}</span>
              <strong>{r.title}</strong>
              <p>{r.snippet}</p>
            </article>
          ))
        : rows.data?.map((r) => (
            <article className="card" key={r.id}>
              <div className="row">
                <strong>{r.title}</strong>
                <span>{r.favorite ? "★" : ""}</span>
              </div>
              <div className="meta">
                {new Date(r.updated_at).toLocaleString()} ·{" "}
                {r.claude_session_id
                  ? "Sesión de Claude guardada"
                  : "Sin sesión de Claude"}
              </div>
              <p>{r.summary}</p>
              <button
                onClick={() =>
                  void request("conversations.open", { id: r.id }).catch((e) =>
                    setError(errorText(e)),
                  )
                }
              >
                Abrir chat
              </button>
              <button
                onClick={() =>
                  void request("conversations.update", {
                    ...r,
                    favorite: r.favorite ? 0 : 1,
                    no_memory: r.no_memory,
                    project_id: r.project_id,
                  })
                    .then(() => rows.reload())
                    .catch((e) => setError(errorText(e)))
                }
              >
                {r.favorite ? "Quitar favorito" : "Favorito"}
              </button>
              <button
                onClick={() => {
                  const title = prompt("Título de la conversación", r.title);
                  if (title)
                    void request("conversations.update", { ...r, title })
                      .then(() => rows.reload())
                      .catch((e) => setError(errorText(e)));
                }}
              >
                Editar título
              </button>
            </article>
          ))}
      {!rows.data?.length && !query && (
        <Empty>Aún no hay conversaciones guardadas.</Empty>
      )}
    </section>
  );
}
function MemoryPanel() {
  const list = useData<Memory[]>("memory.list", {}, "alfredpet:memory");
  const [query, setQuery] = useState("");
  const [content, setContent] = useState("");
  const [type, setType] = useState("fact");
  const [supersedes, setSupersedes] = useState<string | null>(null);
  const [filterType, setFilterType] = useState("");
  const [origin, setOrigin] = useState("all");
  const [exportPath, setExportPath] = useState("");
  const [error, setError] = useState("");
  async function save() {
    try {
      await request("memory.save", {
        content,
        title: content.slice(0, 80),
        type,
        explicit: true,
        supersedes,
      });
      setContent("");
      setSupersedes(null);
      await list.reload();
    } catch (e) {
      setError(errorText(e));
    }
  }
  return (
    <section>
      <h2>Memoria global</h2>
      <p>
        Preferencias y hechos entre conversaciones. Los secretos y el modo
        privacidad bloquean la ingesta.
      </p>
      <MemoryAnalysis />
      <SemanticMemory />
      <input
        aria-label="Buscar memoria"
        placeholder="Filtrar memorias"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      <div className="row">
        <select
          aria-label="Filtrar tipo de memoria"
          value={filterType}
          onChange={(e) => setFilterType(e.target.value)}
        >
          <option value="">Todos los tipos</option>
          {[
            "fact",
            "preference",
            "person",
            "project",
            "decision",
            "procedure",
            "path_resource",
            "recurring_context",
            "lesson",
            "tool_usage_preference",
          ].map((t) => (
            <option key={t}>{t}</option>
          ))}
        </select>
        <select
          aria-label="Filtrar origen de memoria"
          value={origin}
          onChange={(e) => setOrigin(e.target.value)}
        >
          <option value="all">Todos los orígenes</option>
          <option value="explicit">Explícitas</option>
          <option value="inferred">Inferidas</option>
        </select>
        <button
          onClick={() =>
            void request<{ path: string }>("memory.export")
              .then((result) => setExportPath(result.path))
              .catch((e) => setError(errorText(e)))
          }
        >
          Exportar memoria
        </button>
        <button
          onClick={() =>
            void request("memory.rebuild_index")
              .then(() => list.reload())
              .catch((e) => setError(errorText(e)))
          }
        >
          Reconstruir índice
        </button>
      </div>
      {exportPath && <p>Memoria exportada: {exportPath}</p>}
      <div className="row">
        <select
          aria-label="Tipo de memoria"
          value={type}
          onChange={(e) => setType(e.target.value)}
        >
          {[
            "fact",
            "preference",
            "person",
            "project",
            "decision",
            "procedure",
            "path_resource",
            "recurring_context",
            "lesson",
            "tool_usage_preference",
          ].map((t) => (
            <option key={t}>{t}</option>
          ))}
        </select>
        <input
          aria-label="Nueva memoria"
          value={content}
          onChange={(e) => setContent(e.target.value)}
          placeholder="Recuerda esto…"
        />
        <button disabled={!content} onClick={() => void save()}>
          {supersedes ? "Guardar corrección" : "Recordar"}
        </button>
      </div>
      <ErrorNotice text={error || list.error} />
      {list.data
        ?.filter(
          (m) =>
            (!filterType || m.type === filterType) &&
            (origin === "all" ||
              (origin === "explicit" ? !!m.explicit : !m.explicit)) &&
            (m.title + " " + m.content)
              .toLowerCase()
              .includes(query.toLowerCase()),
        )
        .map((m) => (
          <article className={`card ${m.disabled ? "muted" : ""}`} key={m.id}>
            <div className="row">
              <strong>{m.title}</strong>
              <span className="badge">
                {m.explicit ? "Explícita" : "Inferida"} · {m.type}
              </span>
            </div>
            <p>{m.content}</p>
            <div className="meta">
              {m.superseded_by
                ? "Sustituida por una corrección"
                : m.disabled
                  ? "Desactivada"
                  : "Activa"}{" "}
              · {m.source_conversation_id ?? "Usuario"}
            </div>
            <p className="meta">
              Confianza:{" "}
              {Math.round((m.confidence ?? (m.explicit ? 1 : 0.65)) * 100)} % ·
              Usos: {m.use_count ?? 0}
              {m.last_used_at
                ? ` · Último uso: ${new Date(m.last_used_at).toLocaleString()}`
                : ""}
            </p>
            {m.source_conversation_id && (
              <button
                onClick={() =>
                  void request("conversations.open", {
                    id: m.source_conversation_id,
                  }).catch((e) => setError(errorText(e)))
                }
              >
                Ver conversación de origen
              </button>
            )}
            <button
              onClick={() => {
                setSupersedes(m.id);
                setContent(m.content);
                setType(m.type);
              }}
            >
              Corregir
            </button>
            <button
              onClick={() =>
                void request("memory.toggle", {
                  id: m.id,
                  pinned: m.pinned ? 0 : 1,
                  disabled: m.disabled,
                })
                  .then(() => list.reload())
                  .catch((e) => setError(errorText(e)))
              }
            >
              {m.pinned ? "Desfijar" : "Fijar"}
            </button>
            <button
              onClick={() =>
                void request("memory.toggle", {
                  id: m.id,
                  pinned: m.pinned,
                  disabled: m.disabled ? 0 : 1,
                })
                  .then(() => list.reload())
                  .catch((e) => setError(errorText(e)))
              }
            >
              {m.disabled ? "Activar" : "Desactivar"}
            </button>
            <button
              onClick={() =>
                void request("memory.delete", { id: m.id })
                  .then(() => list.reload())
                  .catch((e) => setError(errorText(e)))
              }
            >
              Borrar
            </button>
          </article>
        ))}
    </section>
  );
}
function Settings() {
  const [tab, setTab] = useState("general");
  const modules = useData<Module[]>(
    "modules.list",
    {},
    "alfredpet:modules",
    tab === "modules",
  );
  const settings = useData<{ key: string; json_value: string }[]>(
    "settings.get",
    {},
    "alfredpet:settings",
  );
  const health = useData<{
    version: string;
    approved: boolean;
    diagnostic: string;
    authMethod: string;
    subscriptionType: string;
  }>("claude.health", {}, undefined, tab === "claude");
  const [error, setError] = useState("");
  const [roots, setRoots] = useState<string[]>([]);
  const [audioBusy, setAudioBusy] = useState(false);
  const [modelBusy, setModelBusy] = useState(false);
  const [startup, setStartup] = useState(false);
  const config = Object.fromEntries(
    settings.data?.map((r) => [r.key, JSON.parse(r.json_value) as Json]) ?? [],
  );
  useEffect(() => {
    setRoots((config.allowed_roots as string[] | undefined) ?? []);
  }, [settings.data]);
  useEffect(() => {
    if (desktop)
      void isEnabled()
        .then(setStartup)
        .catch((e) => setError(errorText(e)));
  }, []);
  async function save(key: string, value: Json) {
    try {
      await request("settings.set", { key, value });
      await settings.reload();
    } catch (e) {
      setError(errorText(e));
    }
  }
  async function pickAudio() {
    setAudioBusy(true);
    setError("");
    try {
      const path = await request<string | null>("settings.pick_audio");
      if (path !== null) {
        await request("settings.set", { key: "attention_audio", value: path });
        await settings.reload();
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setAudioBusy(false);
    }
  }
  return (
    <section>
      <div className="tabs">
        {[
          ["general", "General"],
          ["security", "Equipo y seguridad"],
          ["permissions", "Permisos"],
          ["claude", "Claude"],
          ["modules", "Módulos"],
          ["connections", "Conexiones"],
          ["memory", "Memoria"],
          ["projects", "Proyectos"],
          ["files", "Recuperación"],
          ["windows", "Ventanas"],
          ["clipboard", "Portapapeles"],
          ["media", "Documentos e imágenes"],
        ].map(([id, label]) => (
          <button aria-pressed={tab === id} key={id} onClick={() => setTab(id)}>
            {label}
          </button>
        ))}
      </div>
      <ErrorNotice text={error || settings.error} />
      {tab === "security" && <SystemAudit />}
      {tab === "permissions" && <RememberedPermissions />}
      {tab === "general" && (
        <>
          <h1>Configuración</h1>
          <ManualHelp />
          <label className="toggle">
            <input
              type="checkbox"
              checked={config.privacy === true}
              onChange={(e) => void save("privacy", e.target.checked)}
            />
            Privacidad: bloquea capturas, portapapeles, UIA y memoria en backend
          </label>
          <label className="toggle">
            <input
              type="checkbox"
              checked={config.dnd === true}
              onChange={(e) => void save("dnd", e.target.checked)}
            />
            No molestar
          </label>
          <label className="toggle">
            <input
              type="checkbox"
              checked={startup}
              onChange={(e) => {
                const checked = e.target.checked;
                void (checked ? enable() : disable())
                  .then(() => setStartup(checked))
                  .catch((e) => setError(errorText(e)));
              }}
            />
            Iniciar AlfredPet con Windows
          </label>
          <h2>Carpetas permitidas</h2>
          <p>
            Selecciona las carpetas a las que Alfred puede acceder y guarda los
            cambios. Las rutas del sistema, credenciales y junctions permanecen
            protegidas.
          </p>
          <PathsPicker
            label="Carpetas permitidas"
            mode="folder"
            multiple
            append
            value={roots}
            onChange={setRoots}
          />
          <button onClick={() => void save("allowed_roots", roots)}>
            Guardar rutas
          </button>
          <h2>Audio «Señor»</h2>
          <div className="row">
            <button disabled={audioBusy} onClick={() => void pickAudio()}>
              <PixelIcon name="folder" />
              {audioBusy ? "Seleccionando…" : "Seleccionar archivo de audio"}
            </button>
            {!!config.attention_audio && (
              <button
                disabled={audioBusy}
                onClick={() => void save("attention_audio", null)}
              >
                Quitar audio
              </button>
            )}
          </div>
          <p role="status">
            {typeof config.attention_audio === "string" &&
            config.attention_audio
              ? config.attention_audio.split(/[\\/]/).pop()
              : "Ningún archivo seleccionado"}
          </p>
          <p>
            Archivo WAV. Se guarda al seleccionarlo y se usa para avisos, no
            para respuestas normales de chat.
          </p>
          <h2>Datos y diagnósticos</h2>
          <AssetSettings />
          <ProfileBackup />
          <button
            onClick={() =>
              void request<{ path: string }>("backup")
                .then((r) => setError(`Backup: ${r.path}`))
                .catch((e) => setError(errorText(e)))
            }
          >
            Crear backup SQLite
          </button>
          <button
            onClick={() =>
              void request<{ path: string }>("diagnostics.export")
                .then((r) => setError(`Diagnóstico: ${r.path}`))
                .catch((e) => setError(errorText(e)))
            }
          >
            Exportar diagnóstico sin secretos
          </button>
          <p>
            Atajos: Ctrl+Alt+Shift+Space (comandos), Ctrl+Alt+U (explicar texto
            seleccionado), Ctrl+Alt+Mayús+U (región). Búsqueda FTS5 activa.
            Semántica local pendiente de modelo opt-in.
          </p>
          {["Ctrl+Alt+U", "Ctrl+Alt+Shift+U"]
            .filter((key) => config[`shortcut.${key}`] === false)
            .map((key) => (
              <p role="alert" key={key}>
                No se pudo registrar {key}; puede estar ocupado por otra
                aplicación. Libera el atajo y reinicia Alfred. También puedes
                usar Explícame esto → Seleccionar región en el chat.
              </p>
            ))}
        </>
      )}
      {tab === "claude" && (
        <>
          <h1>Claude · suscripción</h1>
          <article className="card claude-model-settings">
            <h2>Modelo y consumo</h2>
            <ClaudeModelSelect
              label="Modelo predeterminado de Claude"
              value={configuredClaudeModel(settings.data)}
              disabled={modelBusy || !settings.data || !!settings.error}
              onChange={(value) => {
                setModelBusy(true);
                void save("claude_model", value).finally(() =>
                  setModelBusy(false),
                );
              }}
            />
            <p>
              Se guarda para los próximos mensajes y análisis automáticos de
              memoria. Puedes cambiarlo también desde el chat; no modifica una
              respuesta en curso.
            </p>
            <dl>
              {claudeModels.map((model) => (
                <div key={model.value}>
                  <dt>{model.label}</dt>
                  <dd>{model.detail}</dd>
                </div>
              ))}
            </dl>
            <p className="meta">
              Comparación orientativa: el consumo depende también del historial,
              los adjuntos y las herramientas. No indica el porcentaje restante
              de tu cuota. La versión concreta y su disponibilidad dependen de
              tu CLI y suscripción.
            </p>
          </article>
          <ErrorNotice text={health.error} />
          <article className="card">
            <strong>
              {!health.data && !health.error
                ? "Comprobando suscripción…"
                : health.data?.approved
                  ? "Suscripción validada"
                  : "Ruta bloqueada"}
            </strong>
            <p>{health.data?.version}</p>
            <p>
              {health.data?.authMethod} · {health.data?.subscriptionType}
            </p>
            <p>{health.data?.diagnostic}</p>
            <button onClick={() => void health.reload()}>Diagnóstico</button>
          </article>
          <p>
            Instala el CLI oficial y ejecuta <code>claude auth login</code> con
            tu cuenta de Claude. AlfredPet no usa claves API, cookies ni OAuth
            propio para Claude.
          </p>
        </>
      )}
      {tab === "modules" && (
        <>
          <h1>Módulos activables</h1>
          <ErrorNotice text={modules.error} />
          {modules.data?.map((m) => (
            <article className="card module" key={m.id}>
              <label>
                <input
                  type="checkbox"
                  checked={m.enabled}
                  onChange={(e) =>
                    void request("modules.toggle", {
                      id: m.id,
                      enabled: e.target.checked,
                    })
                      .then(() => modules.reload())
                      .catch((e) => setError(errorText(e)))
                  }
                />
                <strong>{m.name}</strong>
              </label>
              <span className="meta">
                {m.kind} · {m.tools} herramientas · {m.version}
              </span>
              <span className="badge">{m.live}</span>
            </article>
          ))}
        </>
      )}
      {tab === "modules" && <PluginManager />}
      {tab === "connections" && <Connections />}
      {tab === "memory" && <MemoryPanel />}
      {tab === "projects" && <Projects />}
      {tab === "files" && <FileRecovery />}
      {tab === "windows" && <WindowRecovery />}
      {tab === "clipboard" && <ClipboardPanel />}
      {tab === "media" && <MediaPanel />}
    </section>
  );
}
function Connections() {
  const list = useData<
    {
      id: string;
      module_id: string;
      name: string;
      config_json: string;
      health: string;
    }[]
  >("connections.list");
  const [module, setModule] = useState("notion");
  const [name, setName] = useState("");
  const [config, setConfig] = useState("{}");
  const [secret, setSecret] = useState("");
  const [error, setError] = useState("");
  const [id, setId] = useState<string | null>(null);
  const [driveSaved, setDriveSaved] = useState(false);
  const [restSaved, setRestSaved] = useState(false);
  const [mcpSaved, setMcpSaved] = useState(false);
  const [reauthorize, setReauthorize] = useState(false);
  const selectionEpoch = useRef(0);
  const savePending = useRef(false);
  const [saving, setSaving] = useState(false);
  const databaseTemplates: Record<string, Json> = {
    sqlite: {
      engine: "sqlite",
      path: "",
      read_only: true,
    },
    mysql: {
      engine: "mysql",
      host: "database.example",
      port: 3306,
      database: "configure_database",
      user: "configure_reader",
      driver: "MySQL ODBC 26.7 Unicode Driver",
      ssl: "verify_full",
      ca_path: "",
      read_only: true,
    },
    mariadb: {
      engine: "mariadb",
      host: "database.example",
      port: 3306,
      database: "configure_database",
      user: "configure_reader",
      driver: "MariaDB ODBC 3.2 Driver",
      ssl: "verify_full",
      ca_path: "",
      read_only: true,
    },
    postgresql: {
      engine: "postgresql",
      host: "database.example",
      port: 5432,
      database: "configure_database",
      user: "configure_reader",
      driver: "PostgreSQL Unicode(x64)",
      ssl: "verify_full",
      ca_path: "",
      read_only: true,
    },
    sqlserver: {
      engine: "sqlserver",
      host: "database.example",
      port: 1433,
      database: "configure_database",
      user: "configure_reader",
      driver: "ODBC Driver 18 for SQL Server",
      ssl: "verify_full",
      read_only: true,
    },
  };
  const templates: Record<string, Json> = {
    git: { auth: "prefer_gh", repos: ["owner/repository"] },
    notion: {
      auth: "integration_token",
      api_version: "2025-09-03",
      read_only: true,
      allowed_pages: ["11111111-1111-4111-8111-111111111111"],
      allowed_databases: [],
      allowed_data_sources: [],
    },
    drive: {
      auth: "installed_app_pkce",
      client_id:
        "CONFIGURE_OFFICIAL_DESKTOP_CLIENT_ID.apps.googleusercontent.com",
      client_secret_mode: "none",
      read_only: true,
      allowed_files: [],
      allowed_folders: [],
    },
    homeassistant: {
      profile_version: 1,
      base_url: "https://homeassistant.example",
      read_only: true,
      websocket: false,
      allow_insecure_http: false,
      allowed_entities: [],
      allowed_services: {},
      allowed_events: [],
    },
    rest: {
      profile_version: 1,
      base_url: "https://api.example.com/",
      allowed_hosts: ["api.example.com"],
      read_only: true,
      auth: { type: "none" },
      headers: {},
      operations: {
        status: {
          method: "GET",
          path: "/status",
          path_schema: {
            type: "object",
            properties: {},
            additionalProperties: false,
          },
          query_schema: {
            type: "object",
            properties: {},
            additionalProperties: false,
          },
          body_schema: null,
          response_schema: { type: "object" },
          response: "json",
          success_statuses: [200],
          risk: "free",
        },
      },
      openapi_path: "/openapi.json",
    },
    mcp: {
      profile_version: 1,
      enabled: true,
      transport: "stdio",
      protocol: "auto",
      command: "",
      args: [],
      auth: { type: "none" },
      env: {},
      permissions: { tools: {}, resources: [], prompts: {} },
    },
    database: {
      engine: "sqlite",
      path: "",
      read_only: true,
    },
  };
  async function save() {
    if (savePending.current) return;
    const epoch = selectionEpoch.current;
    savePending.current = true;
    setSaving(true);
    setError("");
    try {
      const result = await request<{ id: string }>("connections.save", {
        id,
        module_id: module,
        name,
        config: JSON.parse(config) as Json,
        secret,
        ...(module === "drive" ? { reauthorize } : {}),
      });
      await list.reload();
      if (selectionEpoch.current !== epoch) return;
      setId(result.id);
      setSecret("");
      setDriveSaved(module === "drive");
      setRestSaved(module === "rest");
      setMcpSaved(module === "mcp");
      setReauthorize(false);
    } catch (e) {
      if (selectionEpoch.current === epoch) {
        const message = errorText(e);
        const pending =
          module === "homeassistant"
            ? message.match(/HA_CONNECTION_CREDENTIAL_PENDING:([a-f0-9-]{36})/)
            : module === "rest"
              ? message.match(
                  /REST_CONNECTION_CREDENTIAL_PENDING:([a-f0-9-]{36})/,
                )
              : module === "mcp"
                ? message.match(
                    /MCP_CONNECTION_CREDENTIAL_PENDING:([a-f0-9-]{36})/,
                  )
                : null;
        if (pending) {
          setId(pending[1]);
          setRestSaved(false);
          setMcpSaved(false);
          await list.reload();
          if (selectionEpoch.current !== epoch) return;
          setError(
            module === "rest" || module === "mcp"
              ? "La conexión quedó pendiente de credenciales. Introduce de nuevo el mapa JSON de secretos y guarda para completar la configuración."
              : "La conexión quedó pendiente de credencial. Introduce de nuevo el token y guarda para completar la configuración.",
          );
        } else setError(message);
      }
    } finally {
      savePending.current = false;
      setSaving(false);
    }
  }
  return (
    <section>
      <h1>Conexiones</h1>
      <p>
        Los secretos se guardan en Windows Credential Manager. Las integraciones
        requieren autenticación oficial del proveedor.
      </p>
      <div className="row">
        <select
          aria-label="Tipo de conexión"
          value={module}
          onChange={(e) => {
            selectionEpoch.current++;
            setModule(e.target.value);
            setConfig(JSON.stringify(templates[e.target.value], null, 2));
            setId(null);
            setSecret("");
            setRestSaved(false);
            setMcpSaved(false);
            setDriveSaved(false);
            setReauthorize(false);
          }}
        >
          {Object.keys(templates).map((m) => (
            <option key={m}>{m}</option>
          ))}
        </select>
        <input
          aria-label="Nombre de conexión"
          disabled={saving}
          placeholder="Nombre"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      {module === "git" && (
        <p>
          GitHub: indica los repositorios permitidos como owner/repository. Se
          usa gh autenticado; el PAT opcional queda en Credential Manager. Las
          mutaciones requieren revisión individual.
        </p>
      )}
      {module === "mcp" && (
        <div>
          <p>
            MCP: selecciona el ejecutable del programa o elige un servidor HTTP.
            Los elementos están denegados hasta que los permitas. Las consultas
            y ejecuciones requieren aprobación individual.
          </p>
          <p>
            La autenticación bearer, los headers privados y las variables env
            usan secret_ref o referencias con nombre. Introduce un mapa JSON de
            valores privados en el campo de credencial. Al cambiar el destino,
            programa, argumentos o referencias, introduce de nuevo ese mapa.
          </p>
          <button
            disabled={saving}
            onClick={() => {
              selectionEpoch.current++;
              setConfig(
                JSON.stringify(
                  {
                    profile_version: 1,
                    enabled: true,
                    transport: "streamable_http",
                    protocol: "auto",
                    base_url: "https://mcp.example",
                    endpoint: "/mcp",
                    auth: { type: "none" },
                    headers: {},
                    permissions: { tools: {}, resources: [], prompts: {} },
                  },
                  null,
                  2,
                ),
              );
              setSecret("");
              setMcpSaved(false);
            }}
          >
            Plantilla MCP HTTP
          </button>
        </div>
      )}
      {module === "rest" && (
        <div>
          <p>
            REST: usa una URL base terminada en / y declara su host en
            allowed_hosts. Las peticiones eligen una operación guardada; cada
            operación define método, ruta, esquemas y riesgo. Solo lectura por
            defecto.
          </p>
          <p>
            auth admite none, bearer, basic y header. Los headers privados y la
            autenticación usan secret_ref. Introduce sus valores como un mapa
            JSON en el campo de credencial, por ejemplo{" "}
            {JSON.stringify({ principal: "tu valor privado" })}; los nombres
            deben coincidir exactamente con las referencias del perfil.
          </p>
          <p>
            Para cambiar destino, autenticación o referencias, introduce las
            credenciales otra vez. Las acciones que modifican datos requieren
            aprobación individual. Guardar permite reintentar una consulta
            bloqueada por límite del proveedor.
          </p>
        </div>
      )}
      {module === "homeassistant" && (
        <div>
          <p>
            Home Assistant: introduce la URL de tu instancia y un token de larga
            duración en el campo de credencial. Solo lectura por defecto. Añade
            las entidades, servicios y eventos que permites consultar.
          </p>
          <p>
            Cada servicio permitido necesita input_schema cerrado y global:
            false para destinos explícitos. Las acciones requieren aprobación
            individual. Para eventos en directo, activa websocket y usa iniciar,
            consultar y detener desde sus herramientas. Las cámaras guardan
            capturas privadas con caducidad.
          </p>
          <p>
            HTTPS por defecto. allow_insecure_http: true permite HTTP de forma
            explícita; el token viajará sin cifrar. Al cambiar la URL, introduce
            una credencial nueva. Una respuesta del servicio no confirma el
            resultado físico de un dispositivo.
          </p>
          <p>
            Si el proveedor limita las solicitudes y pide un reintento manual,
            guarda de nuevo esta conexión para desbloquear la siguiente
            consulta. Guardar no envía acciones al dispositivo.
          </p>
        </div>
      )}
      {module === "drive" && (
        <div>
          <p>
            Drive: configura un cliente oficial de tipo Desktop y los IDs de
            archivos y carpetas permitidos. Solo lectura por defecto. La
            autorización abre el navegador del sistema. Si tu cliente requiere
            secreto, usa client_secret_mode: credential_manager e introdúcelo
            abajo; los tokens se reciben mediante OAuth.
          </p>
          <p>
            Puedes elegir recursos con el selector de Google y guardar sus IDs
            permitidos aquí. El permiso local cubre los hijos directos de las
            carpetas; Google devuelve los recursos con acceso concedido a esta
            aplicación. Las subidas pueden reanudarse desde su registro;
            enviarlos a la papelera requiere una aprobación individual.
          </p>
          {id && (
            <label>
              <input
                type="checkbox"
                checked={reauthorize}
                disabled={saving}
                onChange={(e) => {
                  setReauthorize(e.target.checked);
                  setDriveSaved(false);
                }}
              />
              Reautorizar al cambiar el cliente o migrar esta conexión
            </label>
          )}
        </div>
      )}
      {module === "database" && (
        <div>
          <label>
            Plantilla de motor
            <select
              aria-label="Plantilla de motor de base de datos"
              disabled={saving}
              value=""
              onChange={(e) => {
                selectionEpoch.current++;
                setConfig(
                  JSON.stringify(databaseTemplates[e.target.value], null, 2),
                );
                setSecret("");
              }}
            >
              <option value="">Elegir plantilla…</option>
              {Object.keys(databaseTemplates).map((engine) => (
                <option key={engine} value={engine}>
                  {engine}
                </option>
              ))}
            </select>
          </label>
          <p>
            Solo lectura por defecto. Sustituye los valores de ejemplo y elige
            un driver instalado. verify_full comprueba el certificado y el
            servidor; require cifra sin verificar su identidad. SQLite utiliza
            una ruta permitida. Al cambiar servidor, usuario, driver o TLS de
            una conexión existente, introduce de nuevo la credencial. Las
            escrituras requieren revisión; las exportaciones crean archivos
            nuevos.
          </p>
        </div>
      )}
      <ConnectionConfig
        key={`${module}:${id}:${selectionEpoch.current}`}
        module={module}
        disabled={saving}
        value={config}
        onChange={(value) => {
          setConfig(value);
          setDriveSaved(false);
          setRestSaved(false);
          setMcpSaved(false);
        }}
      />
      <input
        type="password"
        autoComplete="off"
        aria-label="Credencial de conexión"
        disabled={saving}
        placeholder="Token o contraseña · nunca se guarda en SQLite"
        value={secret}
        onChange={(e) => {
          setSecret(e.target.value);
          setDriveSaved(false);
          setRestSaved(false);
          setMcpSaved(false);
        }}
      />
      <button disabled={!name || saving} onClick={() => void save()}>
        Guardar conexión
      </button>
      {module === "mcp" && id && mcpSaved && !saving && (
        <McpInspector
          key={`${id}:${selectionEpoch.current}`}
          connection={id}
          name={name}
          profile={JSON.parse(config) as Json}
          onSaved={(profile) => {
            setConfig(JSON.stringify(profile, null, 2));
            void list.reload();
          }}
        />
      )}
      {module === "mcp" && !mcpSaved && (
        <p>Guarda esta configuración antes de consultar MCP.</p>
      )}
      {module === "rest" && id && restSaved && !saving && (
        <RestOpenApi
          key={`${id}:${selectionEpoch.current}`}
          connection={id}
          onApplied={(profile) => {
            setConfig(JSON.stringify(profile, null, 2));
            void list.reload();
          }}
        />
      )}
      {module === "rest" && !restSaved && (
        <p>Guarda esta configuración antes de importar OpenAPI.</p>
      )}
      {module === "drive" && id && driveSaved && !saving && (
        <DriveAuthorization
          key={`${id}:${selectionEpoch.current}`}
          connection={id}
        />
      )}
      {module === "drive" && !driveSaved && (
        <p>Guarda la configuración antes de autorizar Drive.</p>
      )}
      <ErrorNotice text={error || list.error} />
      {list.data?.map((row) => (
        <article className="card" key={row.id}>
          <strong>{row.name}</strong>
          <span className="badge">
            {row.module_id} · {row.health}
          </span>
          <p>
            <code>{row.id}</code>
          </p>
          <button
            onClick={() => {
              setId(row.id);
              selectionEpoch.current++;
              setSecret("");
              setName(row.name);
              setModule(row.module_id);
              setConfig(JSON.stringify(JSON.parse(row.config_json), null, 2));
              setDriveSaved(row.module_id === "drive");
              setRestSaved(
                row.module_id === "rest" && row.health !== "CREDENTIAL_PENDING",
              );
              setMcpSaved(
                row.module_id === "mcp" &&
                  row.health !== "CREDENTIAL_PENDING" &&
                  row.health !== "CREDENTIAL_REQUIRED",
              );
              setReauthorize(false);
            }}
          >
            Configurar
          </button>
        </article>
      ))}
    </section>
  );
}
function Projects() {
  const list = useData<Project[]>("projects.list");
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [error, setError] = useState("");
  return (
    <section>
      <h1>Proyectos</h1>
      <div className="row">
        <input
          aria-label="Nombre de proyecto"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Nombre"
        />
        <PathPicker
          label="Carpeta de proyecto"
          mode="folder"
          value={path}
          onChange={setPath}
        />
        <button
          disabled={!name || !path}
          onClick={() =>
            void request("projects.save", { name, root_path: path })
              .then(() => {
                setName("");
                setPath("");
                void list.reload();
              })
              .catch((e) => setError(errorText(e)))
          }
        >
          Añadir
        </button>
      </div>
      <ErrorNotice text={error || list.error} />
      {list.data?.map((p) => (
        <article className="card" key={p.id}>
          <strong>{p.name}</strong>
          <p>{p.root_path}</p>
        </article>
      ))}
    </section>
  );
}
function Permissions() {
  const list = useData<Permission[]>(
    "permissions.list",
    {},
    "alfredpet:permission",
  );
  const [error, setError] = useState("");
  async function decide(id: string, decision: string) {
    try {
      await request("permissions.resolve", { id, decision });
      await list.reload();
    } catch (e) {
      setError(errorText(e));
    }
  }
  return (
    <section>
      <h1>Permiso para actuar</h1>
      <UserQuestions />
      <ErrorNotice text={error || list.error} />
      {list.data?.map((p) => (
        <article className="card permission" key={p.id}>
          <strong>{p.tool_id}</strong>
          <span className="badge">
            {p.risk === "Mandatory"
              ? p.rememberable
                ? "L3 · acción sensible"
                : "L3 · confirmación obligatoria"
              : "L2 · contextual"}
          </span>
          <pre>{p.input_json_redacted}</pre>
          <p>
            Revisa los parámetros y el efecto antes de permitir. Se deniega al
            caducar o cerrar la aplicación. «Permitir siempre» conserva sólo
            este alcance; puedes revocarlo en Configuración → Permisos.
          </p>
          <div className="row">
            <button onClick={() => void decide(p.id, "deny")}>Denegar</button>
            <button onClick={() => void decide(p.id, "allow")}>
              Permitir una vez
            </button>
            {p.rememberable && (
              <button onClick={() => void decide(p.id, "always")}>
                Permitir siempre esta acción exacta
              </button>
            )}
            {p.risk !== "Mandatory" && (
              <button onClick={() => void decide(p.id, "session")}>
                Permitir este alcance en la sesión
              </button>
            )}
          </div>
        </article>
      ))}
      {!list.data?.length && <Empty>No hay permisos pendientes.</Empty>}
    </section>
  );
}
function UserQuestions() {
  const questions = useData<
    { id: string; question: string; options_json: string }[]
  >("questions.list", {}, "alfredpet:question");
  const [answer, setAnswer] = useState("");
  const [error, setError] = useState("");
  async function send(id: string, value: string) {
    try {
      await request("questions.answer", { id, answer: value });
      setAnswer("");
      await questions.reload();
    } catch (e) {
      setError(errorText(e));
    }
  }
  return (
    <>
      <ErrorNotice text={error} />
      {questions.data?.map((q) => (
        <article className="card" key={q.id}>
          <h2>{q.question}</h2>
          <div className="row">
            {(JSON.parse(q.options_json) as string[] | null)?.map((option) => (
              <button key={option} onClick={() => void send(q.id, option)}>
                {option}
              </button>
            ))}
          </div>
          <input
            aria-label="Respuesta a Claude"
            value={answer}
            onChange={(e) => setAnswer(e.target.value)}
          />
          <button
            disabled={!answer.trim()}
            onClick={() => void send(q.id, answer)}
          >
            Responder
          </button>
        </article>
      ))}
    </>
  );
}
function ActivityCenter() {
  const list = useData<Activity[]>("activity.list", {}, "alfredpet:activity");
  const [filter, setFilter] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  async function undo(token: string, tool: string) {
    setError("");
    setBusy(token);
    try {
      await request("tools.execute", {
        tool: tool.startsWith("window.") ? "window.undo" : "files.undo",
        input: { id: token },
      });
      await list.reload();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }
  return (
    <section>
      <h1>Activity Center</h1>
      <input
        aria-label="Filtrar actividad"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        placeholder="Filtrar herramienta, riesgo o resultado"
      />
      <ErrorNotice text={error || list.error} />
      {list.data
        ?.filter((a) => `${a.tool_id} ${a.risk} ${a.status}`.includes(filter))
        .map((a) => (
          <article className="card" key={a.id}>
            <div className="row">
              <strong>{a.tool_id}</strong>
              <span className="badge">
                {a.risk} · {a.status}
              </span>
            </div>
            <div className="meta">
              {new Date(a.timestamp).toLocaleString()} · {a.duration_ms} ms
            </div>
            {a.undo_token && a.undo_state === "available" && (
              <button
                disabled={busy !== null}
                onClick={() => void undo(a.undo_token!, a.tool_id)}
              >
                {busy === a.undo_token ? "Esperando confirmación…" : "Deshacer"}
              </button>
            )}
            {a.undo_state === "undone" && (
              <span className="badge">Deshecho</span>
            )}
            <details>
              <summary>Parámetros y resultado</summary>
              <pre>{a.input_summary}</pre>
              <pre>{a.output_summary}</pre>
            </details>
          </article>
        ))}
      {!list.data?.length && (
        <Empty>
          Las acciones reales aparecerán aquí, incluidos permisos denegados.
        </Empty>
      )}
    </section>
  );
}
function Tasks() {
  return (
    <section>
      <TasksPanel />
      <Watches />
      <DownloadPostActions />
      <Organizer />
      <Terminal />
    </section>
  );
}
function Organizer() {
  const [path, setPath] = useState("");
  const [extensions, setExtensions] = useState("");
  const [folder, setFolder] = useState("");
  const [plan, setPlan] = useState<{
    plan_id: string;
    applied: boolean;
    moves: { source: string; destination: string }[];
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function organize(apply: boolean) {
    setBusy(true);
    setError("");
    try {
      const result = await request<NonNullable<typeof plan>>("tools.execute", {
        tool: "files.organize",
        input: {
          path,
          body: apply
            ? { plan_id: plan?.plan_id ?? "" }
            : {
                rules: [
                  {
                    extensions: extensions
                      .split(",")
                      .map((e) => e.trim())
                      .filter(Boolean),
                    folder,
                  },
                ],
              },
        },
      });
      setPlan(result);
    } catch (e) {
      setError(errorText(e));
      if (apply) setPlan(null);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section>
      <h2>Organizar archivos</h2>
      <p>
        Elige una carpeta y una regla. Revisa los movimientos antes de
        aplicarlos; después puedes deshacerlos en Actividad.
      </p>
      <div className="row">
        <PathPicker
          label="Carpeta que organizar"
          mode="folder"
          value={path}
          disabled={busy}
          onChange={(value) => {
            setPath(value);
            setPlan(null);
          }}
        />
        <input
          aria-label="Extensiones que organizar"
          placeholder="pdf, txt, md"
          value={extensions}
          disabled={busy}
          onChange={(e) => {
            setExtensions(e.target.value);
            setPlan(null);
          }}
        />
        <input
          aria-label="Subcarpeta de destino"
          placeholder="Nombre de subcarpeta"
          value={folder}
          disabled={busy}
          onChange={(e) => {
            setFolder(e.target.value);
            setPlan(null);
          }}
        />
        <button
          disabled={
            busy || !path.trim() || !extensions.trim() || !folder.trim()
          }
          onClick={() => void organize(false)}
        >
          Vista previa
        </button>
      </div>
      <ErrorNotice text={error} />
      {plan && (
        <>
          <p>
            {plan.applied ? "Movimientos aplicados" : "Movimientos propuestos"}:{" "}
            {plan.moves.length}
          </p>
          {plan.moves.map((m) => (
            <article className="card" key={m.source}>
              <p>{m.source}</p>
              <p>Destino: {m.destination}</p>
            </article>
          ))}
          {!plan.applied && (
            <button
              disabled={busy || !plan.moves.length}
              onClick={() => void organize(true)}
            >
              Aplicar movimientos
            </button>
          )}
        </>
      )}
    </section>
  );
}
export function Watches() {
  const list = useData<Watch[]>("watches.list", {}, "alfredpet:watches");
  const [kind, setKind] = useState("folder");
  const [path, setPath] = useState("");
  const [recursive, setRecursive] = useState(false);
  const [watchOptions, setWatchOptions] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function execute(tool: string, input: Json) {
    setBusy(true);
    setError("");
    try {
      await request("tools.execute", { tool, input });
      await list.reload();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }
  const labels: Record<string, string> = {
    file: "Archivo",
    folder: "Carpeta",
    download: "Descargas",
    port: "Puerto",
    process: "Proceso",
    window: "Ventana",
    page: "Página del navegador",
    ui_element: "Elemento de interfaz",
  };
  return (
    <section>
      <h2>Vigilancias</h2>
      <p>
        Comprueba cambios periódicamente y conserva los avisos en Actividad. Las
        descargas esperan estabilidad y nunca se ejecutan automáticamente. La
        privacidad suspende las comprobaciones.
      </p>
      <div className="row">
        <select
          aria-label="Tipo de vigilancia"
          value={kind}
          onChange={(e) => {
            const k = e.target.value;
            setKind(k);
            setPath("");
            setWatchOptions(
              k === "page"
                ? JSON.stringify(
                    { tab: "", page_selector: "body", condition: "changed" },
                    null,
                    2,
                  )
                : k === "ui_element"
                  ? JSON.stringify(
                      {
                        process_id: 0,
                        selector: { automation_id: "", scope: "descendants" },
                        condition: "exists",
                      },
                      null,
                      2,
                    )
                  : "",
            );
          }}
        >
          {Object.entries(labels).map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
        {["file", "folder", "download"].includes(kind) ? (
          <PathPicker
            key={kind}
            label="Destino de vigilancia"
            mode={kind === "file" ? "file" : "folder"}
            value={path}
            onChange={setPath}
            disabled={busy}
          />
        ) : (
          <input
            aria-label="Destino de vigilancia"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder={
              kind === "port"
                ? "127.0.0.1:8080"
                : kind === "process"
                  ? "ID del proceso"
                  : kind === "window"
                    ? "HWND de la ventana"
                    : "Nombre de la vigilancia"
            }
          />
        )}
        {["folder", "download"].includes(kind) && (
          <label>
            <input
              type="checkbox"
              checked={recursive}
              onChange={(e) => setRecursive(e.target.checked)}
            />
            Incluir subcarpetas
          </label>
        )}
        <button
          disabled={busy || !path.trim()}
          onClick={() => {
            const owner =
              kind === "download"
                ? "downloads"
                : ["file", "folder"].includes(kind)
                  ? "files"
                  : "monitoring";
            try {
              const body = watchOptions.trim()
                ? JSON.parse(watchOptions)
                : undefined;
              if (
                body !== undefined &&
                (!body || typeof body !== "object" || Array.isArray(body))
              )
                throw new Error(
                  "Opciones de vigilancia: objeto JSON requerido",
                );
              void execute(
                `${owner}.${owner === "monitoring" ? "create" : "watch"}`,
                {
                  path,
                  kind,
                  recursive,
                  ...(body !== undefined ? { body } : {}),
                },
              );
            } catch (e) {
              setError(errorText(e));
            }
          }}
        >
          Crear vigilancia
        </button>
      </div>
      {(kind === "page" || kind === "ui_element") && (
        <label>
          Opciones del destino (JSON)
          <textarea
            aria-label="Opciones de vigilancia"
            value={watchOptions}
            onChange={(e) => setWatchOptions(e.target.value)}
          />
        </label>
      )}
      <ErrorNotice text={error || list.error} />
      {list.data?.map((w) => (
        <article className="card" key={w.id}>
          <strong>
            {labels[w.kind] ?? w.kind}: {w.config.path}
          </strong>
          <span className="badge">
            {!w.enabled
              ? "Pausada"
              : !w.effective_enabled
                ? "Módulo desactivado"
                : w.status === "error"
                  ? "Error"
                  : w.status === "pending"
                    ? "Pendiente"
                    : "Activa"}
          </span>
          {w.error_code && <ErrorNotice text={w.error_code} />}
          <button
            disabled={busy}
            onClick={() =>
              void execute(
                `${w.owner_module}.${w.owner_module === "monitoring" ? "update" : "watch_update"}`,
                { id: w.id, body: { enabled: !w.enabled } },
              )
            }
          >
            {w.enabled ? "Pausar" : "Reanudar"}
          </button>
          <button
            disabled={busy}
            onClick={() =>
              void execute(
                `${w.owner_module}.${w.owner_module === "monitoring" ? "delete" : "unwatch"}`,
                { id: w.id },
              )
            }
          >
            Eliminar vigilancia
          </button>
        </article>
      ))}
      {!list.data?.length && <Empty>No hay vigilancias.</Empty>}
    </section>
  );
}
function Palette() {
  const tools = useData<Tool[]>("tools.list", {}, "alfredpet:modules");
  const recent = useData<Conversation[]>("conversations.list");
  const projects = useData<Project[]>("projects.list");
  const macros = useData<{ id: string; name: string; enabled: number }[]>(
    "macros.list",
    {},
    "alfredpet:macros",
  );
  const [macroId, setMacroId] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    const consume = () =>
      void request<{ id: string } | null>("macros.shortcut_pending")
        .then((value) => {
          if (alive && value?.id) setMacroId(value.id);
        })
        .catch((e) => {
          if (alive) setError(errorText(e));
        });
    void on("alfredpet:macro_shortcut", consume).then((f) => {
      if (alive) unlisten = f;
      else f();
    });
    if (desktop) consume();
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Tool | null>(null);
  const [args, setArgs] = useState("{}");
  const [error, setError] = useState("");
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape" && desktop) void getCurrentWindow().hide();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
  async function run() {
    setBusy(true);
    setError("");
    try {
      if (selected) {
        setResult(
          JSON.stringify(
            await request("tools.execute", {
              tool: selected.id,
              input: JSON.parse(args) as Json,
            }),
            null,
            2,
          ),
        );
      } else {
        const c = await request<{ id: string }>("conversations.create");
        await request("chat.send", { conversation_id: c.id, text: query });
        await request("conversations.open", { id: c.id });
        if (desktop) await getCurrentWindow().hide();
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section>
      <input
        ref={input}
        aria-label="Buscar comando"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Prompt, herramienta, macro o acción…"
        onKeyDown={(e) => {
          if (e.key === "Enter" && !selected) void run();
        }}
      />
      <div className="quick">
        {[
          ["history", "Historial"],
          ["settings", "Configuración"],
          ["capture_overlay", "Capturar"],
          ["task_center", "Tareas"],
        ].map(([id, label]) => (
          <button
            key={id}
            onClick={() => void show(id).catch((e) => setError(errorText(e)))}
          >
            {label}
          </button>
        ))}
      </div>
      <ErrorNotice text={error || tools.error || macros.error} />
      {macroId && (
        <MacroRun
          id={macroId}
          onClose={() => setMacroId(null)}
          onSavedRoutine={() => setMacroId(null)}
          onResult={(r) => setResult(JSON.stringify(r, null, 2))}
        />
      )}
      {!selected && <Routines query={query} />}
      {selected ? (
        <>
          <div className="row">
            <strong>{selected.id}</strong>
            <button onClick={() => setSelected(null)}>Volver</button>
          </div>
          <p>{selected.description}</p>
          <JsonPathEditor
            key={selected.id}
            label="Parámetros de herramienta"
            fields={toolPathFields(
              selected.id,
              selected.inputSchema.required,
              jsonObject(args),
            )}
            value={args}
            onChange={setArgs}
            disabled={busy}
          />
        </>
      ) : (
        <div className="tool-list">
          {tools.data
            ?.filter((t) =>
              `${t.id} ${t.description}`
                .toLowerCase()
                .includes(query.toLowerCase()),
            )
            .map((t) => (
              <button
                key={t.id}
                onClick={() => {
                  setSelected(t);
                  setArgs(
                    JSON.stringify(
                      Object.fromEntries(
                        t.inputSchema.required.map((key) => [
                          key,
                          typeof t.inputSchema.properties[key] === "object" &&
                          t.inputSchema.properties[key] !== null &&
                          !Array.isArray(t.inputSchema.properties[key]) &&
                          (
                            t.inputSchema.properties[key] as Record<
                              string,
                              Json
                            >
                          ).type === "number"
                            ? 0
                            : "",
                        ]),
                      ),
                      null,
                      2,
                    ),
                  );
                }}
              >
                <strong>{t.id}</strong>
                <small>{t.description}</small>
              </button>
            ))}
        </div>
      )}
      <button
        disabled={busy || (!query && !selected)}
        onClick={() => void run()}
      >
        {busy
          ? "Ejecutando…"
          : selected
            ? "Ejecutar herramienta"
            : "Enviar prompt"}
      </button>
      {!selected && (
        <div className="results">
          <h2>Acciones rápidas</h2>
          {[
            ["capture_overlay", "Capturar región"],
            ["task_center", "Tareas"],
            ["history", "Historial"],
          ]
            .filter(
              (item) =>
                !query || item[1].toLowerCase().includes(query.toLowerCase()),
            )
            .map(([surface, label]) => (
              <button
                key={surface}
                onClick={() =>
                  void show(surface).catch((e) => setError(errorText(e)))
                }
              >
                {label}
              </button>
            ))}
          <h2>Conversaciones recientes</h2>
          {recent.data
            ?.filter(
              (c) =>
                !query || c.title.toLowerCase().includes(query.toLowerCase()),
            )
            .slice(0, 8)
            .map((c) => (
              <button
                key={c.id}
                onClick={() =>
                  void request("conversations.open", { id: c.id }).catch((e) =>
                    setError(errorText(e)),
                  )
                }
              >
                {c.title}
              </button>
            ))}
          <h2>Proyectos</h2>
          {projects.data
            ?.filter(
              (p) =>
                !query || p.name.toLowerCase().includes(query.toLowerCase()),
            )
            .map((p) => (
              <button
                key={p.id}
                onClick={() =>
                  void request<{ id: string }>("conversations.create", {
                    project_id: p.id,
                  })
                    .then((c) => request("conversations.open", { id: c.id }))
                    .catch((e) => setError(errorText(e)))
                }
              >
                {p.name}
              </button>
            ))}
          <h2>Macros</h2>
          {macros.data
            ?.filter((m) => m.enabled === 1)
            .filter(
              (m) =>
                !query || m.name.toLowerCase().includes(query.toLowerCase()),
            )
            .map((m) => (
              <button key={m.id} onClick={() => setMacroId(m.id)}>
                {m.name}
              </button>
            ))}
        </div>
      )}
      {result && <pre>{result}</pre>}
    </section>
  );
}
function CaptureOverlay() {
  const [start, setStart] = useState<{ x: number; y: number } | null>(null);
  const [end, setEnd] = useState<{ x: number; y: number } | null>(null);
  const [error, setError] = useState("");
  const [image, setImage] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const oldBackground = document.body.style.background;
    const oldRootBackground = document.documentElement.style.background;
    document.body.style.background = "transparent";
    document.documentElement.style.background = "transparent";
    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape" && desktop) void getCurrentWindow().hide();
    };
    window.addEventListener("keydown", escape);
    let live = true;
    let off: (() => void) | undefined;
    void on("alfredpet:capture_reset", () => {
      setImage("");
      setStart(null);
      setEnd(null);
      setError("");
    }).then((stop) => {
      if (live) off = stop;
      else stop();
    });
    return () => {
      live = false;
      off?.();
      window.removeEventListener("keydown", escape);
      document.body.style.background = oldBackground;
      document.documentElement.style.background = oldRootBackground;
    };
  }, []);
  async function capture() {
    if (!start || !end || !desktop) return;
    const width = Math.abs(end.x - start.x),
      height = Math.abs(end.y - start.y);
    if (width < 5 || height < 5) return;
    setBusy(true);
    setError("");
    const w = getCurrentWindow();
    try {
      const pos = await w.outerPosition(),
        scale = await w.scaleFactor();
      await w.hide();
      await new Promise((resolve) => setTimeout(resolve, 150));
      const result = await request<{ id: string }>("tools.execute", {
        tool: "vision.capture_region",
        input: {
          x: Math.round(pos.x + Math.min(start.x, end.x) * scale),
          y: Math.round(pos.y + Math.min(start.y, end.y) * scale),
          width: Math.round(width * scale),
          height: Math.round(height * scale),
        },
      });
      setImage(result.id);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      await w.show();
    }
  }
  const rect =
    start && end
      ? {
          left: Math.min(start.x, end.x),
          top: Math.min(start.y, end.y),
          width: Math.abs(end.x - start.x),
          height: Math.abs(end.y - start.y),
        }
      : null;
  return (
    <div
      className="capture-overlay"
      onPointerDown={(e) => {
        if (
          busy ||
          (e.target as HTMLElement).closest("button, .capture-result")
        )
          return;
        e.currentTarget.setPointerCapture(e.pointerId);
        setImage("");
        setStart({ x: e.clientX, y: e.clientY });
        setEnd({ x: e.clientX, y: e.clientY });
      }}
      onPointerMove={(e) => {
        if (e.buttons === 1 && start) setEnd({ x: e.clientX, y: e.clientY });
      }}
    >
      <div className="capture-toolbar">
        <strong>AlfredPet · arrastra para seleccionar una región</strong>
        <button
          disabled={busy || !rect || rect.width < 5 || rect.height < 5}
          onClick={() => void capture()}
        >
          {busy ? "Capturando…" : "Capturar selección"}
        </button>
        <button
          onClick={() => {
            if (desktop) void getCurrentWindow().hide();
          }}
        >
          Cancelar · Esc
        </button>
        <ErrorNotice text={error} />
      </div>
      {rect && !image && <div className="capture-selection" style={rect} />}
      {image && (
        <div className="capture-result">
          <CaptureAsset key={image} id={image} onDeleted={() => setImage("")} />
          <button
            disabled={busy}
            onClick={() => {
              setBusy(true);
              setError("");
              void request("explain.asset", { id: image })
                .then(() => setImage(""))
                .catch((e) => setError(explainError(e)))
                .finally(() => setBusy(false));
            }}
          >
            Explicar en el chat
          </button>
          <button
            onClick={() => {
              setImage("");
              setStart(null);
              setEnd(null);
            }}
          >
            Nueva selección
          </button>
        </div>
      )}
    </div>
  );
}
function Bubble() {
  const [text, setText] = useState("AlfredPet");
  useEffect(() => {
    void request<{ event: string }>("attention.last")
      .then((e) => setText(e.event ?? "AlfredPet"))
      .catch(() => {});
    let off: (() => void) | undefined;
    void on<{ event: string }>("alfredpet:attention", (e) =>
      setText(e.event),
    ).then((f) => {
      off = f;
    });
    return () => off?.();
  }, []);
  return (
    <section
      className="app surface-bubble"
      style={visualStyle as CSSProperties}
    >
      <p>{text}</p>
      <button onClick={() => void show("chat")}>
        <PixelIcon name="chat" /> Abrir chat
      </button>
    </section>
  );
}
