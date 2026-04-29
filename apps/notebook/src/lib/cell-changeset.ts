/**
 * CellChangeset types and merge utilities.
 *
 * Re-exported from the `runtimed` package. This module exists so existing
 * app imports (`../lib/cell-changeset`) continue to work without changes.
 */
export {
  classifyCellChangesetMaterialization,
  type CellChangeset,
  type CellChangesetMaterialization,
  type ChangedCell,
  type ChangedFields,
  mergeChangesets,
} from "runtimed";
