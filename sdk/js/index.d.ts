// Types of hydo-assert (CommonJS entry). The wire format and verdict rules: hydo/ASSERTIONS.md.

/** A details value: encoded as Go's encoding/json encodes the corresponding Go value. */
export type DetailValue =
  | null
  | undefined
  | boolean
  | number
  | bigint
  | string
  | Uint8Array
  | DetailValue[]
  | ReadonlyArray<DetailValue>
  | Map<unknown, DetailValue>
  | Set<DetailValue>
  | { toJSON(): DetailValue }
  | { [key: string]: DetailValue };

/** The free-form details object of an evaluation. Absent or empty: no "details" field. */
export type Details = { [key: string]: DetailValue } | Map<string, DetailValue> | null | undefined;

export type CmpOp = '<' | '<=' | '==' | '!=' | '>=' | '>';

/** One declared property. */
export declare class Assertion {
  private constructor();
  readonly id: string;
  readonly msg: string;
  /** Evaluates always / alwaysOrUnreachable / sometimes; returns the condition as a boolean. */
  check(cond: unknown, details?: Details): boolean;
  /** Marks a reachable (ok:true) or unreachable (ok:false, a violation) location as reached. */
  hit(details?: Details): void;
  /** Evaluates `left op right` as a check; left/op/right are added to the details. */
  cmp(left: number | bigint, op: CmpOp, right: number | bigint, details?: Details): boolean;
}

/** One episode of a Liveness property, started at its anchor. */
export declare class Watch {
  private constructor();
  /** The condition became true now: ok if within the deadline. Returns whether it was in time. */
  done(details?: Details): boolean;
  /** The deadline passed with the condition still false (a violation). */
  expire(details?: Details): void;
}

/** A declared bounded-liveness (eventually) property. */
export declare class Liveness {
  private constructor();
  readonly id: string;
  readonly withinMs: number;
  /** Starts an episode now (the anchor). */
  begin(): Watch;
  /** Polls cond every pollMs until true or the deadline passed; one record; resolves to the outcome. */
  awaitCond(cond: () => boolean | Promise<boolean>, pollMs: number, details?: Details): Promise<boolean>;
  /** As awaitCond, blocking the thread between evaluations. */
  awaitCondSync(cond: () => boolean, pollMs: number, details?: Details): boolean;
}

/** A declared BUGGIFY fault point. */
export declare class Point {
  private constructor();
  readonly id: string;
  /** Whether the point misbehaves on this evaluation (always false without a BUGGIFY seed). */
  fire(details?: Details): boolean;
}

export declare function always(id: string, msg: string): Assertion;
export declare function alwaysOrUnreachable(id: string, msg: string): Assertion;
export declare function sometimes(id: string, msg: string): Assertion;
export declare function reachable(id: string, msg: string): Assertion;
export declare function unreachable(id: string, msg: string): Assertion;
export declare function eventually(id: string, msg: string, withinMs: number): Liveness;
export declare function buggify(id: string, msg: string): Point;
/** Emits one HYDO-ASSERT-DECL line per declared assertion, in declaration order. */
export declare function catalog(): void;
/** Emits HYDO-SETUP-COMPLETE. */
export declare function setupComplete(): void;
