import { Fragment, useEffect, useLayoutEffect, useRef } from "react";
import { ChatProgress, type ChatRun } from "./ChatProgress";
import { ChatCopyButton } from "./ChatCopyButton";
import type { Revision } from "./ChatRevision";
import { ChatMessageContent } from "./ChatMessageContent";
import type { Attachment, Message } from "../types";
import { ConversationAttachments } from "./ManagedAssets";
import { identity } from "../identity";

const noReplies: Message[] = [];
const noProgress: ChatRun[] = [];
const noAttachments: Attachment[] = [];

function Avatar({ role }: { role: string }) {
  const user = role === "user";
  return (
    <img
      className="message-avatar"
      src={user ? identity.userAvatar : identity.assistantAvatar}
      alt={user ? identity.userLabel : identity.assistantLabel}
      width={24}
      height={24}
    />
  );
}

export function ChatTranscript({
  conversationId,
  messages,
  stream,
  pendingText,
  replies = noReplies,
  progress = noProgress,
  actionsDisabled = false,
  onRevise,
  attachments = noAttachments,
  onAttachmentsChanged,
}: {
  conversationId: string;
  messages: Message[] | null;
  stream: string;
  pendingText: string;
  replies?: Message[];
  progress?: ChatRun[];
  actionsDisabled?: boolean;
  onRevise?: (revision: Revision) => void;
  attachments?: Attachment[];
  onAttachmentsChanged?: () => void;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const following = useRef(true);

  // Follow each new chunk and the persisted reply before the browser paints.
  useLayoutEffect(() => {
    following.current = true;
    const node = viewport.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [
    conversationId,
    messages,
    stream,
    pendingText,
    replies,
    progress,
    attachments,
  ]);

  useEffect(() => {
    const node = viewport.current;
    const body = content.current;
    if (!node || !body || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      // Keep the end visible after wrapping/resizing, but let users read history
      // between responses without a resize pulling them away from their place.
      if (following.current) node.scrollTop = node.scrollHeight;
    });
    observer.observe(node);
    observer.observe(body);
    return () => observer.disconnect();
  }, []);

  return (
    <div
      className="transcript"
      aria-live="polite"
      ref={viewport}
      onScroll={(event) => {
        const node = event.currentTarget;
        following.current =
          node.scrollHeight - node.clientHeight - node.scrollTop <= 2;
      }}
    >
      <div className="transcript-content" ref={content}>
        {!messages?.length && !stream && !pendingText && (
          <div className="empty">
            AlfredPet está listo para conversar.
            <br />
            Claude usa tu suscripción y las herramientas pasan por permisos.
          </div>
        )}
        {[
          ...(messages ?? []).map(
            (message) => replies.find((r) => r.id === message.id) ?? message,
          ),
          ...replies.filter((r) => !messages?.some((m) => m.id === r.id)),
        ]
          .filter(
            (message) =>
              message.content_text ||
              attachments.some((row) => row.message_id === message.id),
          )
          .map((message) => (
            <Fragment key={message.id}>
              <article className={`message ${message.role}`}>
                <Avatar role={message.role} />
                <div className="meta">
                  <time
                    dateTime={message.created_at}
                    title={new Date(message.created_at).toLocaleString()}
                  >
                    {new Date(message.created_at).toLocaleString([], {
                      day: "2-digit",
                      month: "short",
                      hour: "2-digit",
                      minute: "2-digit",
                      second: "2-digit",
                    })}
                  </time>
                  {message.status === "streaming" && " · respondiendo"}
                  {message.status === "interrupted" && " · interrumpido"}
                  {message.status === "error" && " · error"}
                </div>
                <ChatMessageContent text={message.content_text} />
                {attachments.some((row) => row.message_id === message.id) && (
                  <ConversationAttachments
                    sent
                    conversationId={conversationId}
                    rows={attachments.filter(
                      (row) => row.message_id === message.id,
                    )}
                    busy={actionsDisabled}
                    onChanged={() => onAttachmentsChanged?.()}
                  />
                )}
                <div className="message-actions">
                  <ChatCopyButton text={message.content_text} />
                  {onRevise && ["user", "assistant"].includes(message.role) && (
                    <button
                      disabled={
                        actionsDisabled || message.status === "streaming"
                      }
                      title={
                        actionsDisabled
                          ? "Termina la acción en curso y envía o vacía el borrador"
                          : "Conservar el original y abrir una variante"
                      }
                      onClick={() =>
                        onRevise({
                          message,
                          mode: message.role === "user" ? "edit" : "regenerate",
                          text:
                            message.role === "user" ? message.content_text : "",
                        })
                      }
                    >
                      {message.role === "user"
                        ? "Editar y reenviar"
                        : "Regenerar"}
                    </button>
                  )}
                </div>
              </article>
              {progress
                .filter((r) => r.message_id === message.id)
                .map((r) => (
                  <ChatProgress key={r.id} run={r} />
                ))}
            </Fragment>
          ))}
        {progress
          .filter((r) => !messages?.some((m) => m.id === r.message_id))
          .map((r) => (
            <ChatProgress key={r.id} run={r} />
          ))}
        {stream && (
          <article className="message assistant">
            <Avatar role="assistant" />
            <div className="meta">respondiendo</div>
            <ChatMessageContent text={stream} />
          </article>
        )}
        {pendingText && (
          <div className="chat-working" role="status">
            <span className="working-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            <span>{pendingText}</span>
          </div>
        )}
      </div>
    </div>
  );
}
