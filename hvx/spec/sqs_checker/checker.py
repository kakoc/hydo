"""A history checker for SQS-like queue semantics.

Input: a history of client operations, each with an invoke time and a completion time on the
SAME clock (under hydo: the guest's virtual clock, so times are deterministic), and an outcome:
  ok      the operation definitely took effect, with the returned value
  fail    the operation definitely did not take effect
  info    unknown (timeout, connection lost): it may or may not have taken effect

Operations (dicts):
  {"op": "send",    "id": <client-unique message key>, "group": g|None, "dedup": d|None,
   "t0": invoke, "t1": complete, "outcome": ...}
  {"op": "receive", "t0", "t1", "outcome": "ok", "msgs": [{"id", "receipt", "vis": seconds-as-ms}]}
  {"op": "delete",  "receipt", "t0", "t1", "outcome": ...}

The checker accepts every behaviour the semantics ALLOW (duplicates under at-least-once, any
order on standard queues, an unknown send appearing or not) and reports only behaviours no
correct implementation could produce. Wherever an exact server-side moment is unknown it uses
the conservative bound: a violation is reported only if it holds for every point inside the
operations' [t0, t1] windows.

Semantics knobs (`Spec`): which promises the queue makes. `Spec.sqs_standard()` is what AWS
documents for standard queues (at-least-once; visibility and delete are best effort);
`Spec.sqs_fifo()` adds FIFO ordering, group locking and the deduplication window;
`Spec.strict()` is the stronger contract a purpose-built broker might promise.
"""

from collections import defaultdict
from dataclasses import dataclass


@dataclass
class Spec:
    fifo: bool = False
    strict_visibility: bool = False   # no second delivery while a receipt's visibility is running
    strict_delete: bool = False       # a successfully deleted message never comes back
    dedup_window_ms: int = 5 * 60 * 1000
    retention_ms: int | None = None   # never delivered after send + retention
    require_drain: bool = True        # history ends with a drain: every acked send must be seen

    @staticmethod
    def sqs_standard(**kw):
        return Spec(fifo=False, **kw)

    @staticmethod
    def sqs_fifo(**kw):
        return Spec(fifo=True, **kw)

    @staticmethod
    def strict(fifo=False, **kw):
        return Spec(fifo=fifo, strict_visibility=True, strict_delete=True, **kw)


def check(history, spec=Spec()):
    """Return {"valid": bool, "errors": [str], "stats": {...}}."""
    errors = []
    sends = [o for o in history if o["op"] == "send"]
    attempted = {o["id"] for o in sends if o["outcome"] != "fail"}
    acked = {o["id"]: o for o in sends if o["outcome"] == "ok"}
    send_by_id = defaultdict(list)
    for o in sends:
        send_by_id[o["id"]].append(o)

    # deliveries: (msg id, receipt, receive op); deletes keyed by receipt
    deliveries = []
    for o in history:
        if o["op"] == "receive" and o["outcome"] == "ok":
            for m in o["msgs"]:
                deliveries.append((m["id"], m["receipt"], m.get("vis", 0), o))
    receipt_of = {r: (mid, op) for mid, r, _, op in deliveries}
    deletes = [o for o in history if o["op"] == "delete" and o["outcome"] == "ok"]

    # 1. no phantom messages: everything delivered was attempted
    for mid, _, _, op in deliveries:
        if mid not in attempted:
            errors.append(f"unexpected: message {mid} delivered at t={op['t1']} was never sent")

    # 2. no loss: an acked send must be delivered at least once (needs a drain at the end)
    seen = {mid for mid, *_ in deliveries}
    if spec.require_drain:
        for mid, o in acked.items():
            if mid not in seen and not (spec.fifo and _dedup_covered(o, sends, seen, spec)):
                errors.append(f"lost: message {mid} acked at t={o['t1']} was never delivered")

    by_msg = defaultdict(list)
    for mid, r, vis, op in deliveries:
        by_msg[mid].append((op["t0"], op["t1"], vis, r))
    for lst in by_msg.values():
        lst.sort()

    # 3. visibility: a later delivery that completed before the earlier one's visibility could
    #    have ended (earliest server moment t0 + vis), with no ChangeMessageVisibility modelled
    if spec.strict_visibility:
        for mid, lst in by_msg.items():
            for (a0, a1, vis, ra), (b0, b1, _, rb) in zip(lst, lst[1:]):
                if b1 < a0 + vis and not _deleted_by(deletes, ra, b1):
                    errors.append(f"visibility: message {mid} delivered again at t<={b1} while "
                                  f"receipt {ra} (t>={a0}, vis {vis}) was still invisible")

    # 4. delete is final: no delivery from a receive invoked after a successful delete completed
    if spec.strict_delete:
        for d in deletes:
            if d["receipt"] not in receipt_of:
                continue
            mid, _ = receipt_of[d["receipt"]]
            for b0, b1, _, rb in by_msg[mid]:
                if b0 > d["t1"]:
                    errors.append(f"resurrected: message {mid} delivered at t={b1} after its "
                                  f"delete completed at t={d['t1']}")

    # 5. retention: not delivered after (latest possible send moment) + retention
    if spec.retention_ms is not None:
        for mid, lst in by_msg.items():
            latest_send = max(o["t1"] for o in send_by_id[mid]) if send_by_id[mid] else 0
            for b0, b1, _, _ in lst:
                if b0 > latest_send + spec.retention_ms:
                    errors.append(f"retention: message {mid} delivered at t>={b0}, past "
                                  f"retention ({latest_send}+{spec.retention_ms})")

    if spec.fifo:
        errors += _check_fifo(sends, by_msg, deliveries, deletes, spec)

    return {"valid": not errors, "errors": errors,
            "stats": {"sends": len(sends), "acked": len(acked), "delivered": len(deliveries),
                      "distinct_delivered": len(seen), "deletes": len(deletes)}}


def _dedup_covered(o, sends, seen, spec):
    """A FIFO send acked as a duplicate: another send with its dedup id, possibly inside the
    window, was delivered, so the queue legitimately kept only that copy."""
    d = o.get("dedup")
    if d is None:
        return False
    return any(s is not o and s.get("dedup") == d and s["id"] in seen and s["outcome"] != "fail"
               and abs(o["t1"] - s["t0"]) < spec.dedup_window_ms for s in sends)


def _deleted_by(deletes, receipt, before):
    return any(d["receipt"] == receipt and d["t1"] <= before for d in deletes)


def _check_fifo(sends, by_msg, deliveries, deletes, spec):
    errors = []
    # 6. deduplication: two sends with the same dedup id inside the window enqueue one message.
    #    Distinct message ids sharing a dedup id within the window must not both be delivered.
    by_dedup = defaultdict(list)
    for o in sends:
        if o["outcome"] != "fail" and o.get("dedup") is not None:
            by_dedup[o["dedup"]].append(o)
    delivered = set(by_msg)
    for d, lst in by_dedup.items():
        lst.sort(key=lambda o: o["t0"])
        for a, b in zip(lst, lst[1:]):
            # definitely inside the window: b's latest moment minus a's earliest moment < window
            if a["id"] != b["id"] and b["t1"] - a["t0"] < spec.dedup_window_ms \
                    and a["id"] in delivered and b["id"] in delivered:
                errors.append(f"dedup: sends {a['id']} and {b['id']} share dedup id {d} within "
                              f"the window but both were delivered")

    # 7. per-group order: within a group, first deliveries follow the order of definitely
    #    ordered acked sends (send a completed before send b was invoked)
    group_of = {o["id"]: o.get("group") for o in sends}
    first = {mid: lst[0] for mid, lst in by_msg.items()}
    groups = defaultdict(list)
    for o in sends:
        if o["outcome"] == "ok" and o.get("group") is not None and o["id"] in first:
            groups[o["group"]].append(o)
    for g, lst in groups.items():
        for a in lst:
            for b in lst:
                if a["t1"] < b["t0"]:                      # a definitely sent before b
                    fa, fb = first[a["id"]], first[b["id"]]
                    if fb[1] < fa[0]:                      # b definitely delivered before a
                        errors.append(f"order: group {g}: {b['id']} delivered before {a['id']}, "
                                      f"which was sent first")

    # 8. group lock: while a delivered message of a group is invisible and not deleted, no later
    #    message of that group may be delivered
    del_t = {d["receipt"]: d["t0"] for d in deletes}
    by_group = defaultdict(list)
    for mid, r, vis, op in deliveries:
        g = group_of.get(mid)
        if g is not None:
            by_group[g].append((op["t0"], op["t1"], vis, r, mid))
    for g, lst in by_group.items():
        lst.sort()
        for i, (a0, a1, vis, ra, ma) in enumerate(lst):
            # the hold ends at the earliest of: visibility expiry (latest bound) and delete start
            hold_end = a1 + vis
            if ra in del_t:
                hold_end = min(hold_end, del_t[ra])
            for b0, b1, _, rb, mb in lst[i + 1:]:
                if mb != ma and b1 < hold_end and b0 > a1:
                    errors.append(f"group lock: group {g}: {mb} delivered at t<={b1} while {ma} "
                                  f"(receipt {ra}) was still in flight")
    return errors
