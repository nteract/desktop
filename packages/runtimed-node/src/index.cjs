/**
 * @runtimed/node — thin JS wrapper over the N-API binding.
 *
 * The native binding lives in `binding.cjs` + `runtimed-node.<triple>.node`.
 * This file exists so consumers can `require('@runtimed/node')`
 * without thinking about platform suffixes.
 */
"use strict";

const binding = require("./binding.cjs");

module.exports = binding;
