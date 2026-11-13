-- hydo-msg: every queued transaction ends exactly once: one copy that is APPLIED, INFLIGHT or REJECTED, never none and never two
-- Rule 1.3 (RESULTS.md). Blnk saves a transaction as QUEUED; its worker writes the outcome as
-- a copy with the reference <reference>_q whose parent is the QUEUED row. Read after
-- eventually_1_settle, which waits for the queue and runs Blnk's queue recovery.
SELECT p.reference, count(c.id)::text AS final_copies,
       coalesce(string_agg(c.reference || ' ' || c.status, ', ' ORDER BY c.reference), 'none') AS copies
FROM blnk.transactions p
LEFT JOIN blnk.transactions c
  ON c.parent_transaction = p.transaction_id AND c.status IN ('APPLIED', 'INFLIGHT', 'REJECTED')
WHERE p.status = 'QUEUED'
GROUP BY p.reference
HAVING count(c.id) <> 1 OR bool_or(c.reference <> p.reference || '_q')
