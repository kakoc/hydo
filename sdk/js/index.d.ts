// Types of hydo-assert (CommonJS entry). The wire format and verdict rules:
// https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md

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

/** The operator of {@link Assertion.cmp}. */
export type CmpOp = '<' | '<=' | '==' | '!=' | '>=' | '>';

/** One declared property. Create it with {@link always}, {@link sometimes} and the other declaring functions. */
export declare class Assertion {
  private constructor();
  /** The property's id, as its records carry it. */
  readonly id: string;
  /** The property's message for people. */
  readonly msg: string;
  /**
   * Checks an `always`, `alwaysOrUnreachable` or `sometimes` property and returns the condition
   * as a boolean. Each outcome is written once per process; a false `sometimes` writes nothing.
   * @example ACKED.check(got === want, { key, got });
   */
  check(cond: unknown, details?: Details): boolean;
  /**
   * Marks a `reachable` (written with ok:true) or `unreachable` (written with ok:false, a
   * violation) location as reached.
   */
  hit(details?: Details): void;
  /**
   * Checks `left op right` as {@link Assertion.check} does, with `left`, `op` and `right` added
   * to the details. Values that cannot be compared make the condition false.
   * @example LOG_SIZE.cmp(size, '<=', 10000);
   */
  cmp(left: number | bigint, op: CmpOp, right: number | bigint, details?: Details): boolean;
}

/** One episode of a {@link Liveness} property, started by {@link Liveness.begin}. Only its first `done` or `expire` counts. */
export declare class Watch {
  private constructor();
  /** The condition became true now: ok if within the deadline. Returns whether it was in time. */
  done(details?: Details): boolean;
  /** The deadline passed with the condition still false (a violation). */
  expire(details?: Details): void;
}

/** A declared `eventually` property: after an anchor, a condition becomes true within a deadline. */
export declare class Liveness {
  private constructor();
  /** The property's id. */
  readonly id: string;
  /** The deadline of an episode, in milliseconds after its start. */
  readonly withinMs: number;
  /** Starts an episode now (the anchor). */
  begin(): Watch;
  /**
   * Starts an episode now, evaluates `cond` every `pollMs` (with `setTimeout`, so the event loop
   * keeps running) until it is true or the deadline has passed, writes one record and resolves
   * to the outcome. A condition that becomes true exactly at the deadline still counts.
   * @example await BACK.awaitCond(async () => (await ping()) === 'pong', 100);
   */
  awaitCond(cond: () => boolean | Promise<boolean>, pollMs: number, details?: Details): Promise<boolean>;
  /** As {@link Liveness.awaitCond}, but blocks the thread between evaluations (`Atomics.wait`). */
  awaitCondSync(cond: () => boolean, pollMs: number, details?: Details): boolean;
}

/**
 * A declared fault point (BUGGIFY): a place where the program misbehaves on purpose when hydo
 * turns the point on for a run. Its catalog entry is the `sometimes` property `buggify.<id>`.
 */
export declare class Point {
  private constructor();
  /** The point's id (without the `buggify.` prefix of its catalog entry). */
  readonly id: string;
  /**
   * Whether the point misbehaves on this call. Always false, with no other effect, when no
   * BUGGIFY seed is configured (`HYDO_BUGGIFY`, or the file `HYDO_BUGGIFY_FILE` names).
   * @example if (LOST_REPLY.fire({ key })) return;
   */
  fire(details?: Details): boolean;
}

/**
 * Declares a property that must hold every time it is checked, and be checked at least once.
 * Declare properties at the top level of a module, so that {@link catalog} lists them.
 * @param id a stable id, for example `'kv.acked_durable'`
 * @param msg what the property says, for people
 * @example const ACKED = ha.always('kv.acked_durable', 'every acknowledged write reads back');
 */
export declare function always(id: string, msg: string): Assertion;
/** Declares a property that must hold every time it is checked; never being checked is fine. */
export declare function alwaysOrUnreachable(id: string, msg: string): Assertion;
/** Declares a property that must be true at least once across an exploration. */
export declare function sometimes(id: string, msg: string): Assertion;
/** Declares a code location that must be reached at least once across an exploration ({@link Assertion.hit}). */
export declare function reachable(id: string, msg: string): Assertion;
/** Declares a code location that must never be reached ({@link Assertion.hit} is a violation). */
export declare function unreachable(id: string, msg: string): Assertion;
/**
 * Declares an `eventually` property: after an anchor, a condition becomes true within
 * `withinMs` milliseconds of guest time.
 * @example const BACK = ha.eventually('kv.back', 'the server answers within 5 s', 5000);
 */
export declare function eventually(id: string, msg: string, withinMs: number): Liveness;
/**
 * Declares a fault point (BUGGIFY).
 * @example const LOST_REPLY = ha.buggify('kv.lost_reply', 'the write lands but the reply is lost');
 */
export declare function buggify(id: string, msg: string): Point;
/**
 * Writes one `HYDO-ASSERT-DECL` line per declared property, in declaration order, so that hydo
 * can report the properties that were never evaluated. Call it once, early, in the process
 * whose console hydo reads.
 */
export declare function catalog(): void;
/** Writes `HYDO-SETUP-COMPLETE`: the system is ready, and hydo may start injecting faults. */
export declare function setupComplete(): void;
