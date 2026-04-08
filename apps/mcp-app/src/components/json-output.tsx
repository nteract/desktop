interface JsonOutputProps {
  data: unknown;
}

export function JsonOutput({ data }: JsonOutputProps) {
  let formatted: string;
  try {
    const obj = typeof data === "string" ? JSON.parse(data) : data;
    formatted = JSON.stringify(obj, null, 2);
  } catch {
    formatted = String(data);
  }
  return <pre className="json-output">{formatted}</pre>;
}
