// ESM entry of hydo-assert: re-exports the CommonJS module, so a program that loads the SDK
// both with require() and with import uses ONE state (one catalog, one dedup table, one sink).
import m from './index.js';

export const {
  always,
  alwaysOrUnreachable,
  sometimes,
  reachable,
  unreachable,
  eventually,
  buggify,
  catalog,
  setupComplete,
  Assertion,
  Liveness,
  Watch,
  Point,
} = m;
export default m;
