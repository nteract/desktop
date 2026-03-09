/**
 * Renderer for application/vnd.imo.stat+json MIME type.
 */

export interface ImoStatData {
  value: string;
  label?: string;
  caption?: string;
  direction?: "increase" | "decrease";
  bordered?: boolean;
  target_direction?: "increase" | "decrease";
}

export function ImoStat({ data }: { data: ImoStatData }) {
  const isDark = document.documentElement.classList.contains("dark");

  // Determine direction indicator color
  let directionColor: string | undefined;
  let directionArrow: string | undefined;
  if (data.direction) {
    const isGood = data.direction === (data.target_direction ?? "increase");
    directionColor = isGood ? "#10b981" : "#ef4444";
    directionArrow = data.direction === "increase" ? "\u25B2" : "\u25BC";
  }

  return (
    <div
      style={{
        display: "inline-flex",
        flexDirection: "column",
        padding: "12px 16px",
        minWidth: "100px",
        ...(data.bordered
          ? {
              border: `1px solid ${isDark ? "#374151" : "#e5e7eb"}`,
              borderRadius: "8px",
            }
          : {}),
      }}
    >
      {data.label && (
        <div
          style={{
            fontSize: "12px",
            fontWeight: 500,
            color: isDark ? "#9ca3af" : "#6b7280",
            textTransform: "uppercase",
            letterSpacing: "0.05em",
            marginBottom: "4px",
          }}
        >
          {data.label}
        </div>
      )}
      <div
        style={{
          fontSize: "24px",
          fontWeight: 700,
          lineHeight: 1.2,
          color: isDark ? "#f3f4f6" : "inherit",
        }}
      >
        {data.value}
        {directionArrow && (
          <span
            style={{
              fontSize: "12px",
              marginLeft: "6px",
              color: directionColor,
            }}
          >
            {directionArrow}
          </span>
        )}
      </div>
      {data.caption && (
        <div
          style={{
            fontSize: "12px",
            color: isDark ? "#6b7280" : "#9ca3af",
            marginTop: "4px",
          }}
        >
          {data.caption}
        </div>
      )}
    </div>
  );
}
