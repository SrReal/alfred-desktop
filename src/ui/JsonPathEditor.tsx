import { useRef, useState } from "react";
import { PathPicker, PathsPicker, type PathMode } from "./PathPicker";
import type { Json } from "../types";

export type PathField = {
  key: string;
  label: string;
  mode: PathMode | "source";
  multiple?: boolean;
};
export function jsonObject(value: string): Record<string, Json> | null {
  try {
    const parsed: Json = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed
      : null;
  } catch {
    return null;
  }
}

export function PathFieldControl({
  field,
  value,
  disabled,
  onChange,
}: {
  field: PathField;
  value: Json | undefined;
  disabled: boolean;
  onChange: (value: Json) => void;
}) {
  const [sourceMode, setSourceMode] = useState<PathMode>("file");
  return (
    <div className="row">
      {field.mode === "source" && (
        <select
          aria-label={`Tipo de ${field.label}`}
          value={sourceMode}
          disabled={disabled}
          onChange={(e) => setSourceMode(e.target.value as PathMode)}
        >
          <option value="file">Archivo</option>
          <option value="folder">Carpeta</option>
        </select>
      )}
      <PathsPicker
        label={field.label}
        mode={field.mode === "source" ? sourceMode : field.mode}
        multiple={field.multiple}
        disabled={disabled}
        value={
          field.multiple
            ? Array.isArray(value)
              ? value.filter((v): v is string => typeof v === "string")
              : []
            : typeof value === "string" && value
              ? [value]
              : []
        }
        onChange={(paths) =>
          onChange(field.multiple ? paths : (paths[0] ?? ""))
        }
      />
    </div>
  );
}

// Paths are held separately while the remaining JSON is temporarily invalid.
// Selecting a path only edits the draft; the caller retains review/save controls.
export function JsonPathEditor({
  value,
  onChange,
  fields,
  label,
  disabled = false,
}: {
  value: string;
  onChange: (value: string) => void;
  fields: PathField[];
  label: string;
  disabled?: boolean;
}) {
  const remembered = useRef<Record<string, Json>>({});
  const object = jsonObject(value);
  const remainder = object ? { ...object } : null;
  if (object) {
    remembered.current = Object.fromEntries(
      fields.filter((f) => f.key in object).map((f) => [f.key, object[f.key]]),
    );
    for (const field of fields) delete remainder![field.key];
  }
  return (
    <>
      {fields.map((field) => (
        <PathFieldControl
          key={field.key}
          field={field}
          value={remembered.current[field.key]}
          disabled={disabled || !object}
          onChange={(path) =>
            onChange(JSON.stringify({ ...object, [field.key]: path }, null, 2))
          }
        />
      ))}
      <textarea
        aria-label={label}
        disabled={disabled}
        spellCheck={false}
        value={
          remainder && fields.length
            ? JSON.stringify(remainder, null, 2)
            : value
        }
        onChange={(e) => {
          const next = jsonObject(e.target.value);
          onChange(
            next && fields.length
              ? JSON.stringify({ ...next, ...remembered.current }, null, 2)
              : e.target.value,
          );
        }}
      />
    </>
  );
}

export function ConnectionConfig({
  module,
  value,
  onChange,
  disabled,
}: {
  module: string;
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  const config = jsonObject(value);
  // Keep the editor mounted while JSON is incomplete so selections survive edits.
  const lastFields = useRef<PathField[]>([]);
  if (config)
    lastFields.current =
      module === "database"
        ? config.engine === "sqlite"
          ? [{ key: "path", label: "Archivo de base de datos", mode: "file" }]
          : ["mysql", "mariadb", "postgresql"].includes(String(config.engine))
            ? [{ key: "ca_path", label: "Certificado CA", mode: "file" }]
            : []
        : module === "mcp" && config.transport === "stdio"
          ? [{ key: "command", label: "Ejecutable MCP", mode: "file" }]
          : [];
  return (
    <>
      <JsonPathEditor
        value={value}
        onChange={onChange}
        fields={lastFields.current}
        label="Configuración de conexión"
        disabled={disabled}
      />
      {module === "mcp" && config?.transport === "stdio" && (
        <PathPicker
          label="Añadir archivo a argumentos MCP"
          mode="file"
          value=""
          disabled={disabled}
          onChange={(path) => {
            if (path)
              onChange(
                JSON.stringify(
                  {
                    ...config,
                    args: [
                      ...(Array.isArray(config.args) ? config.args : []),
                      path,
                    ],
                  },
                  null,
                  2,
                ),
              );
          }}
        />
      )}
    </>
  );
}

// Explicit filesystem contracts only: registry keys, URLs and remote IDs stay text.
export function toolPathFields(
  tool: string,
  required: string[],
  input: Record<string, Json> | null = null,
): PathField[] {
  const [module, operation] = tool.split(".");
  const fields: PathField[] = [];
  const add = (
    key: string,
    label: string,
    mode: PathField["mode"],
    multiple = false,
  ) => {
    if (required.includes(key)) fields.push({ key, label, mode, multiple });
  };
  if (
    ["documents", "images", "printers", "vision", "terminal"].includes(module)
  )
    add("path", "Archivo de origen", "file");
  if (["documents", "images"].includes(module)) {
    add("paths", "Archivos de origen", "file", true);
    add(
      "destination",
      operation === "compare"
        ? "Archivo para comparar"
        : "Guardar resultado como",
      operation === "compare" ? "file" : "save",
    );
  }
  if (module === "terminal") add("cwd", "Carpeta del terminal", "folder");
  if (tool === "drive.upload" && (!input?.action || input.action === "start"))
    fields.push({
      key: "path",
      label: "Archivo para subir a Drive",
      mode: "file",
    });
  if (module === "git" || module === "custom")
    add("path", "Carpeta de origen", "folder");
  if (module === "vscode")
    add(
      "path",
      "Archivo o proyecto",
      operation === "run" ? "folder" : "source",
    );
  if (module === "database")
    add(
      "path",
      operation === "export"
        ? "Guardar exportación como"
        : "Archivo para importar",
      operation === "export" ? "save" : "file",
    );
  if (module === "downloads")
    add(
      "path",
      "Destino de descargas",
      ["list", "watch"].includes(operation) ? "folder" : "file",
    );
  if (module === "files") {
    add(
      "path",
      "Origen",
      ["list", "find", "organize", "duplicates"].includes(operation)
        ? "folder"
        : operation === "write"
          ? "save"
          : ["read", "extract"].includes(operation)
            ? "file"
            : "source",
    );
    add("destination", "Destino", operation === "compare" ? "source" : "save");
  }
  if (
    module === "monitoring" &&
    ["file", "folder", "download"].includes(String(input?.kind))
  )
    add(
      "path",
      "Destino de vigilancia",
      input?.kind === "file" ? "file" : "folder",
    );
  return fields;
}
