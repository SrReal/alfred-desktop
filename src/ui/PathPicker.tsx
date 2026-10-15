import { useEffect, useRef, useState } from "react";
import { request } from "../api";
import { PixelIcon } from "./PixelIcon";

export type PathMode = "file" | "folder" | "save";
type Props = {
  label: string;
  mode: PathMode;
  value: string[];
  onChange: (paths: string[]) => void;
  multiple?: boolean;
  append?: boolean;
  disabled?: boolean;
  extensions?: string[];
  defaultName?: string;
};

export function PathsPicker({
  label,
  mode,
  value,
  onChange,
  multiple = false,
  append = false,
  disabled = false,
  extensions = [],
  defaultName = "",
}: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const pending = useRef(false);
  const epoch = useRef(0);
  const latest = useRef({ value, onChange, disabled });
  latest.current = { value, onChange, disabled };
  const identity = JSON.stringify([
    label,
    mode,
    multiple,
    extensions,
    defaultName,
  ]);
  useEffect(() => {
    ++epoch.current;
    setError("");
    return () => {
      ++epoch.current;
    };
  }, [identity]);
  async function choose() {
    if (pending.current || disabled) return;
    const current = epoch.current;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      const paths = await request<string[] | null>("paths.pick", {
        mode,
        title: label,
        multiple,
        extensions,
        default_name: defaultName,
      });
      if (
        current !== epoch.current ||
        latest.current.disabled ||
        paths === null
      )
        return;
      if (
        !Array.isArray(paths) ||
        !paths.length ||
        paths.some((path) => typeof path !== "string" || !path.trim()) ||
        (!multiple && paths.length !== 1)
      )
        throw new Error("El selector no ha devuelto una selección válida.");
      latest.current.onChange(
        append ? [...new Set([...latest.current.value, ...paths])] : paths,
      );
    } catch (e) {
      if (current === epoch.current)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }
  const names = value.map(
    (path) => path.split(/[\\/]/).filter(Boolean).pop() || path,
  );
  return (
    <span className="path-picker">
      <span className="path-picker-controls">
        <button
          type="button"
          aria-label={label}
          title={value.length ? value.join("\n") : label}
          disabled={disabled || busy}
          onClick={() => void choose()}
        >
          <PixelIcon name={mode === "folder" ? "folder" : "file"} />
          <span>
            {busy ? "Seleccionando…" : multiple ? label : names[0] || label}
          </span>
        </button>
        {!multiple && value.length > 0 && (
          <button
            type="button"
            className="path-picker-clear"
            aria-label={`Quitar ${label}`}
            disabled={disabled || busy}
            onClick={() => {
              setError("");
              onChange([]);
            }}
          >
            <PixelIcon name="remove" />
          </button>
        )}
      </span>
      {multiple && value.length > 0 && (
        <span className="path-picker-list">
          {value.map((path, index) => (
            <span key={`${index}:${path}`} className="path-picker-item">
              <span title={path}>{names[index]}</span>
              <button
                type="button"
                aria-label={`Quitar ${names[index]}`}
                disabled={disabled || busy}
                onClick={() => onChange(value.filter((_, i) => i !== index))}
              >
                <PixelIcon name="remove" />
              </button>
            </span>
          ))}
        </span>
      )}
      {error && <span role="alert">{error}</span>}
    </span>
  );
}

export function PathPicker({
  value,
  onChange,
  ...props
}: Omit<Props, "value" | "onChange" | "multiple" | "append"> & {
  value: string;
  onChange: (path: string) => void;
}) {
  return (
    <PathsPicker
      {...props}
      value={value ? [value] : []}
      onChange={(paths) => onChange(paths[0] ?? "")}
    />
  );
}
