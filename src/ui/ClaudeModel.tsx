import type { Json } from "../types";

export const claudeModels = [
  {
    value: "auto",
    label: "Automático · según Claude",
    detail:
      "Conserva la elección de Claude. Una conversación reanudada puede mantener su modelo anterior.",
  },
  {
    value: "haiku",
    label: "Haiku · menor consumo",
    detail: "Consultas rápidas y tareas sencillas. Suele gastar menos cuota.",
  },
  {
    value: "sonnet",
    label: "Sonnet · equilibrado",
    detail: "Equilibrio entre capacidad y consumo para el uso habitual.",
  },
  {
    value: "opus",
    label: "Opus · mayor consumo",
    detail: "Razonamiento y tareas difíciles. Suele gastar más cuota.",
  },
] as const;

export function configuredClaudeModel(
  rows: { key: string; json_value: string }[] | null,
) {
  const row = Array.isArray(rows)
    ? rows.find((r) => r.key === "claude_model")
    : undefined;
  if (!row) return "auto";
  try {
    const value: Json = JSON.parse(row.json_value) as Json;
    return claudeModels.some((m) => m.value === value) ? String(value) : "auto";
  } catch {
    return "auto";
  }
}

export function ClaudeModelSelect({
  value,
  disabled,
  onChange,
  label = "Modelo de Claude",
}: {
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
  label?: string;
}) {
  return (
    <select
      className="model-select"
      aria-label={label}
      value={value}
      disabled={disabled}
      title="Modelo para los próximos mensajes y análisis de memoria. Consumo orientativo; disponibilidad según tu suscripción."
      onChange={(e) => onChange(e.target.value)}
    >
      {claudeModels.map((model) => (
        <option key={model.value} value={model.value}>
          {model.label}
        </option>
      ))}
    </select>
  );
}
