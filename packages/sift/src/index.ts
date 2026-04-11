/**
 * @nteract/data-explorer — public API
 *
 * React component:
 *   import { SiftTable } from '@nteract/data-explorer'
 *   <SiftTable url="/data.arrow" onChange={handleState} />
 *
 * Imperative engine:
 *   import { createTable } from '@nteract/data-explorer'
 *   const engine = createTable(container, tableData)
 *
 * State serialization:
 *   import { engineStateToExplorerState, predicateToSQL } from '@nteract/data-explorer'
 */

export type { SummaryAccumulator } from "./accumulators";
// Accumulators (for custom data pipelines)
export {
  BooleanAccumulator,
  CategoricalAccumulator,
  detectColumnType,
  formatCell,
  isNullSentinel,
  NumericAccumulator,
  refineColumnType,
  TimestampAccumulator,
} from "./accumulators";
export type {
  BetweenPredicate,
  ColumnPredicate,
  CompoundPredicate,
  ContainsPredicate,
  EqPredicate,
  ExplorerState,
  FilterPredicate,
  InPredicate,
  IsNullPredicate,
  NotPredicate,
  SortEntry,
} from "./filter-schema";
// Filter schema & state serialization
export {
  columnFiltersToPredicates,
  explorerStateToJSON,
  predicateToEnglish,
  predicateToPandas,
  predicateToSQL,
} from "./filter-schema";
export type { SiftTableHandle, SiftTableProps } from "./react";
// React component
export { SiftTable, useSiftEngine } from "./react";
export type {
  BooleanColumnSummary,
  BooleanFilter,
  CategoricalColumnSummary,
  Column,
  ColumnFilter,
  ColumnSummary,
  ColumnType,
  NumericColumnSummary,
  RangeFilter,
  SetFilter,
  TableData,
  TableEngine,
  TableEngineOptions,
  TableEngineState,
  TimestampColumnSummary,
} from "./table";
// Imperative engine
export { createTable } from "./table";
