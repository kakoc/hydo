-- hydo-msg: a balance is the sum of its applied transactions: its credit is what was applied to it, its debit what was applied from it
-- Rule 1.2 (RESULTS.md). The same sums as Blnk's own trigger reconcile_balance_from_transactions.
WITH s AS (
  SELECT b.balance_id, b.indicator, b.credit_balance, b.debit_balance,
         coalesce((SELECT sum(t.precise_amount) FROM blnk.transactions t
                   WHERE t.destination = b.balance_id AND t.status = 'APPLIED'), 0) AS cr,
         coalesce((SELECT sum(t.precise_amount) FROM blnk.transactions t
                   WHERE t.source = b.balance_id AND t.status = 'APPLIED'), 0) AS dr
  FROM blnk.balances b)
SELECT coalesce(indicator, balance_id) AS balance,
       credit_balance::text || ' credit, applied to it ' || cr::text AS credit,
       debit_balance::text || ' debit, applied from it ' || dr::text AS debit
FROM s WHERE credit_balance <> cr OR debit_balance <> dr
