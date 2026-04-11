/**
 * Predicate schema for interactive data exploration filters.
 *
 * Designed to be:
 * - Emitted by UI interactions (brush, click, toggle)
 * - Stored in Automerge for collaboration/persistence
 * - Read by AI to understand user intent
 * - Compiled to pandas, polars, SQL, Arrow compute, or JS filter functions
 */

// --- Predicate types ---

export type BetweenPredicate = {
  column: string;
  op: "between";
  value: [number, number]; // [min, max] inclusive
};

export type EqPredicate = {
  column: string;
  op: "eq";
  value: string | number | boolean | null;
};

export type NeqPredicate = {
  column: string;
  op: "neq";
  value: string | number | boolean | null;
};

export type InPredicate = {
  column: string;
  op: "in";
  value: (string | number | null)[];
};

export type NotInPredicate = {
  column: string;
  op: "not_in";
  value: (string | number | null)[];
};

export type GtPredicate = {
  column: string;
  op: "gt";
  value: number;
};

export type GtePredicate = {
  column: string;
  op: "gte";
  value: number;
};

export type LtPredicate = {
  column: string;
  op: "lt";
  value: number;
};

export type LtePredicate = {
  column: string;
  op: "lte";
  value: number;
};

export type IsNullPredicate = {
  column: string;
  op: "is_null";
};

export type IsNotNullPredicate = {
  column: string;
  op: "is_not_null";
};

export type ContainsPredicate = {
  column: string;
  op: "contains";
  value: string;
};

export type ColumnPredicate =
  | BetweenPredicate
  | EqPredicate
  | NeqPredicate
  | InPredicate
  | NotInPredicate
  | GtPredicate
  | GtePredicate
  | LtPredicate
  | LtePredicate
  | IsNullPredicate
  | IsNotNullPredicate
  | ContainsPredicate;

export type CompoundPredicate = {
  op: "and" | "or";
  args: FilterPredicate[];
};

export type NotPredicate = {
  op: "not";
  arg: FilterPredicate;
};

export type FilterPredicate = ColumnPredicate | CompoundPredicate | NotPredicate;

// --- Explorer state (the full picture for AI / Automerge) ---

export type SortEntry = {
  column: string;
  direction: "asc" | "desc";
};

export type ExplorerState = {
  filters: FilterPredicate[]; // top-level AND
  sort: SortEntry[];
  resultCount?: number;
  totalCount?: number;
};

// --- Compilers ---

export function predicateToSQL(p: FilterPredicate): string {
  if ("column" in p) {
    const col = quoteIdent(p.column);
    switch (p.op) {
      case "between":
        return `${col} BETWEEN ${literal(p.value[0])} AND ${literal(p.value[1])}`;
      case "eq":
        return p.value === null ? `${col} IS NULL` : `${col} = ${literal(p.value)}`;
      case "neq":
        return p.value === null ? `${col} IS NOT NULL` : `${col} != ${literal(p.value)}`;
      case "in":
        return `${col} IN (${p.value.map(literal).join(", ")})`;
      case "not_in":
        return `${col} NOT IN (${p.value.map(literal).join(", ")})`;
      case "gt":
        return `${col} > ${literal(p.value)}`;
      case "gte":
        return `${col} >= ${literal(p.value)}`;
      case "lt":
        return `${col} < ${literal(p.value)}`;
      case "lte":
        return `${col} <= ${literal(p.value)}`;
      case "is_null":
        return `${col} IS NULL`;
      case "is_not_null":
        return `${col} IS NOT NULL`;
      case "contains":
        return `${col} LIKE ${literal(`%${p.value}%`)}`;
    }
  }
  if (p.op === "not") return `NOT (${predicateToSQL(p.arg)})`;
  return `(${p.args.map(predicateToSQL).join(p.op === "and" ? " AND " : " OR ")})`;
}

export function predicateToPandas(p: FilterPredicate, dfName = "df"): string {
  if ("column" in p) {
    const col = `${dfName}["${p.column}"]`;
    switch (p.op) {
      case "between":
        return `${col}.between(${p.value[0]}, ${p.value[1]})`;
      case "eq":
        return p.value === null ? `${col}.isna()` : `(${col} == ${pyLiteral(p.value)})`;
      case "neq":
        return p.value === null ? `${col}.notna()` : `(${col} != ${pyLiteral(p.value)})`;
      case "in":
        return `${col}.isin(${pyList(p.value)})`;
      case "not_in":
        return `~${col}.isin(${pyList(p.value)})`;
      case "gt":
        return `(${col} > ${p.value})`;
      case "gte":
        return `(${col} >= ${p.value})`;
      case "lt":
        return `(${col} < ${p.value})`;
      case "lte":
        return `(${col} <= ${p.value})`;
      case "is_null":
        return `${col}.isna()`;
      case "is_not_null":
        return `${col}.notna()`;
      case "contains":
        return `${col}.str.contains(${pyLiteral(p.value)})`;
    }
  }
  if (p.op === "not") return `~(${predicateToPandas(p.arg, dfName)})`;
  const joiner = p.op === "and" ? " & " : " | ";
  return `(${p.args.map((a) => predicateToPandas(a, dfName)).join(joiner)})`;
}

export function predicateToEnglish(p: FilterPredicate): string {
  if ("column" in p) {
    const col = p.column;
    switch (p.op) {
      case "between":
        return `${col} is between ${p.value[0]} and ${p.value[1]}`;
      case "eq":
        return p.value === null ? `${col} is null` : `${col} is ${p.value}`;
      case "neq":
        return p.value === null ? `${col} is not null` : `${col} is not ${p.value}`;
      case "in":
        return `${col} is one of ${(p.value as (string | number)[]).join(", ")}`;
      case "not_in":
        return `${col} is not one of ${(p.value as (string | number)[]).join(", ")}`;
      case "gt":
        return `${col} > ${p.value}`;
      case "gte":
        return `${col} >= ${p.value}`;
      case "lt":
        return `${col} < ${p.value}`;
      case "lte":
        return `${col} <= ${p.value}`;
      case "is_null":
        return `${col} is null`;
      case "is_not_null":
        return `${col} is not null`;
      case "contains":
        return `${col} contains "${p.value}"`;
    }
  }
  if (p.op === "not") return `not (${predicateToEnglish(p.arg)})`;
  const joiner = p.op === "and" ? " and " : " or ";
  return p.args.map(predicateToEnglish).join(joiner);
}

// --- Conversion from our internal ColumnFilter types ---

import type { ColumnFilter, TableEngineState } from "./table";

export function columnFiltersToPredicates(
  columns: { key: string }[],
  filters: ColumnFilter[],
): FilterPredicate[] {
  const predicates: FilterPredicate[] = [];
  for (let i = 0; i < columns.length; i++) {
    const f = filters[i];
    if (!f) continue;
    const col = columns[i].key;
    switch (f.kind) {
      case "range":
        predicates.push({ column: col, op: "between", value: [f.min, f.max] });
        break;
      case "set":
        predicates.push({ column: col, op: "in", value: [...f.values] });
        break;
      case "boolean":
        predicates.push({ column: col, op: "eq", value: f.value });
        break;
    }
  }
  return predicates;
}

/**
 * Convert a TableEngineState (from the engine API) to a portable ExplorerState
 * suitable for Automerge persistence, AI consumption, or cross-system compilation.
 */
export function engineStateToExplorerState(state: TableEngineState): ExplorerState {
  const filters: FilterPredicate[] = [];
  for (const { column, filter } of state.filters) {
    if (!filter) continue;
    switch (filter.kind) {
      case "range":
        filters.push({
          column,
          op: "between",
          value: [filter.min, filter.max],
        });
        break;
      case "set":
        filters.push({ column, op: "in", value: [...filter.values] });
        break;
      case "boolean":
        filters.push({ column, op: "eq", value: filter.value });
        break;
    }
  }

  const sort: SortEntry[] = state.sort
    ? [{ column: state.sort.column, direction: state.sort.direction }]
    : [];

  return {
    filters,
    sort,
    resultCount: state.filteredCount,
    totalCount: state.totalCount,
  };
}

export function explorerStateToJSON(state: ExplorerState): string {
  return JSON.stringify(state, null, 2);
}

// --- Helpers ---

function quoteIdent(s: string): string {
  return `"${s.replace(/"/g, '""')}"`;
}

function literal(v: unknown): string {
  if (v === null) return "NULL";
  if (typeof v === "string") return `'${v.replace(/'/g, "''")}'`;
  return String(v);
}

function pyLiteral(v: unknown): string {
  if (v === null) return "None";
  if (typeof v === "boolean") return v ? "True" : "False";
  if (typeof v === "string") return `"${v.replace(/"/g, '\\"')}"`;
  return String(v);
}

function pyList(values: (string | number | null)[]): string {
  return `[${values.map(pyLiteral).join(", ")}]`;
}
