// ESM entry of hydo-assert/history: re-exports the CommonJS module, so a program that loads the
// recorder both with require() and with import uses ONE sink.
import m from './history.js';

export const {
  NIL,
  History,
  Op,
  history,
  enc,
  ver,
  valueOrNil,
  list,
  append,
  read,
  readResult,
} = m;
export default m;
