import { useCallback, useMemo, useState, useRef, type ReactNode } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { request } from "../api";
import { ChatCopyButton } from "./ChatCopyButton";
import { ChatAssetImage } from "./ManagedAssets";

export function webUrl(raw: string): string | undefined {
  if (!/^https?:\/\//i.test(raw) || /[\s\u0000-\u001f\u007f]/.test(raw)) return;
  try {
    const url = new URL(raw);
    if (!url.hostname || url.username || url.password) return;
    return url.href;
  } catch {
    return;
  }
}
const assetId = (raw: string) =>
  /^alfred-asset:([a-f0-9-]{36})$/i.exec(raw)?.[1];

function ChatImage({ src, alt }: { src: string; alt: string }) {
  const [failed, setFailed] = useState(false);
  const id = assetId(src);
  if (id) return <ChatAssetImage id={id} alt={alt} />;
  const url = webUrl(src);
  if (!url || !url.startsWith("https:") || failed)
    return (
      <span className="chat-image-unavailable">
        {alt || "Imagen"} · imagen no disponible
      </span>
    );
  return (
    <img
      className="chat-image"
      src={url}
      alt={alt}
      loading="lazy"
      referrerPolicy="no-referrer"
      onError={() => setFailed(true)}
    />
  );
}

function CodeBlock({ children }: { children: ReactNode }) {
  const code = useRef<HTMLPreElement>(null);
  return (
    <div className="chat-code">
      <ChatCopyButton
        text={() => code.current?.textContent ?? ""}
        label="Copiar código"
      />
      <pre ref={code}>{children}</pre>
    </div>
  );
}

export function ChatMessageContent({ text }: { text: string }) {
  const [error, setError] = useState("");
  const open = useCallback(async (url: string) => {
    setError("");
    try {
      await request("chat.open_link", { url });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  const components = useMemo<Components>(
    () => ({
      a: ({ href, children }) =>
        href ? (
          <a
            href={href}
            title={href}
            onClick={(e) => {
              e.preventDefault();
              void open(href);
            }}
            onAuxClick={(e) => {
              e.preventDefault();
              if (e.button === 1) void open(href);
            }}
          >
            {children}
          </a>
        ) : (
          <span>{children}</span>
        ),
      img: ({ src, alt }) => (
        <ChatImage
          key={String(src)}
          src={typeof src === "string" ? src : ""}
          alt={alt || ""}
        />
      ),
      pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
      table: ({ children }) => (
        <div className="chat-table">
          <table>{children}</table>
        </div>
      ),
    }),
    [open],
  );
  return (
    <div className="message-content">
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        urlTransform={(url, key) =>
          key === "src" && assetId(url) ? url : webUrl(url) || ""
        }
        components={components}
      >
        {text}
      </Markdown>
      {error && (
        <span role="alert" className="error">
          No se pudo abrir el enlace: {error}
        </span>
      )}
    </div>
  );
}
