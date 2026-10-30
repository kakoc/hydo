// Types of hydo-assert/history (CommonJS entry). The wire format and checkers: hydo/HISTORIES.md.

/** A record argument: a string that is already one token (see enc), or an integer. */
export type Token = string | number | bigint;

/** The value of an absent key (register model). */
export declare const NIL: 'nil';

/** One named history (a check's `history:`; [A-Za-z0-9_.-]+). */
export declare class History {
  constructor(name: string);
  readonly name: string;
  /**
   * Records the invocation of f(args...) by process `proc` (an integer, or a token without
   * spaces) and returns the operation. Record it before sending the request.
   */
  invoke(proc: number | bigint | string, f: string, ...args: Token[]): Op;
  /** Writes one raw record (`type` is invoke, ok, fail or info). */
  record(proc: number | bigint | string, type: 'invoke' | 'ok' | 'fail' | 'info' | string, f: string, ...args: Token[]): void;
}

/** An invoked operation waiting for its completion. */
export declare class Op {
  private constructor();
  readonly history: History;
  readonly proc: string;
  readonly f: string;
  readonly args: string[];
  /** A successful completion; with no args the invocation's are repeated. */
  ok(...args: Token[]): void;
  /** The operation definitely had no effect. */
  fail(): void;
  /** The outcome is unknown (a timeout, a lost connection). */
  info(): void;
}

/** new History(name). */
export declare function history(name: string): History;

/** One token from a value: Go's hydohist.Enc on the UTF-8 bytes. */
export declare function enc(v: string | number | bigint): string;
/** "enc(v)@version" (version: an int64). */
export declare function ver(v: string | number | bigint, version: number | bigint): string;
/**
 * enc(v) if present, else NIL. With one argument, null and undefined are absent; with two, the
 * second says whether the value is present (Go's ValOrNil(v, ok)).
 */
export declare function valueOrNil(v: string | number | bigint | null | undefined, ok?: boolean): string;
/** "[e1,e2,...]" with each element encoded. */
export declare function list(...elems: Array<string | number | bigint>): string;
/** The txn micro-op "a:<k>:<e>". */
export declare function append(k: string | number | bigint, e: string | number | bigint): string;
/** The txn micro-op "r:<k>" (invocation). */
export declare function read(k: string | number | bigint): string;
/** The txn micro-op "r:<k>:[...]" (completion). */
export declare function readResult(k: string | number | bigint, ...l: Array<string | number | bigint>): string;
