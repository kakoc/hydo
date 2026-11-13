-- hydo-msg: a balance's held amounts are the sum of its open holds: what each INFLIGHT hold held, less what was committed and voided from it
-- Rule 1.2, the inflight half (RESULTS.md). A hold is its INFLIGHT row; each commit is an
-- APPLIED row and a void a VOID row whose parent is the hold.
WITH h AS (
  SELECT t.transaction_id, t.source, t.destination,
         t.precise_amount
         - coalesce((SELECT sum(c.precise_amount) FROM blnk.transactions c
                     WHERE c.parent_transaction = t.transaction_id AND c.status IN ('APPLIED', 'VOID')), 0) AS open
  FROM blnk.transactions t WHERE t.status = 'INFLIGHT'),
s AS (
  SELECT b.balance_id, b.indicator, b.inflight_credit_balance, b.inflight_debit_balance,
         coalesce((SELECT sum(open) FROM h WHERE h.destination = b.balance_id), 0) AS icr,
         coalesce((SELECT sum(open) FROM h WHERE h.source = b.balance_id), 0) AS idr
  FROM blnk.balances b)
SELECT coalesce(indicator, balance_id) AS balance,
       inflight_credit_balance::text || ' held for it, open holds ' || icr::text AS held_for,
       inflight_debit_balance::text || ' held from it, open holds ' || idr::text AS held_from
FROM s WHERE inflight_credit_balance <> icr OR inflight_debit_balance <> idr
