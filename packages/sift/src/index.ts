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

// React component
export { SiftTable, useSiftEngine } from './react'
export type { SiftTableProps, SiftTableHandle } from './react'

// Imperative engine
export { createTable } from './table'
export type {
  TableEngine,
  TableEngineState,
  TableEngineOptions,
  TableData,
  Column,
  ColumnType,
  ColumnSummary,
  NumericColumnSummary,
  CategoricalColumnSummary,
  BooleanColumnSummary,
  TimestampColumnSummary,
  ColumnFilter,
  RangeFilter,
  SetFilter,
  BooleanFilter,
} from './table'

// Accumulators (for custom data pipelines)
export {
  detectColumnType,
  refineColumnType,
  isNullSentinel,
  formatCell,
  NumericAccumulator,
  TimestampAccumulator,
  CategoricalAccumulator,
  BooleanAccumulator,
} from './accumulators'
export type { SummaryAccumulator } from './accumulators'

// Filter schema & state serialization
export {
  columnFiltersToPredicates,
  explorerStateToJSON,
  predicateToSQL,
  predicateToPandas,
  predicateToEnglish,
} from './filter-schema'
export type {
  ExplorerState,
  SortEntry,
  FilterPredicate,
  ColumnPredicate,
  CompoundPredicate,
  NotPredicate,
  BetweenPredicate,
  EqPredicate,
  InPredicate,
  ContainsPredicate,
  IsNullPredicate,
} from './filter-schema'
