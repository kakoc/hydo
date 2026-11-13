-- hydo-msg: every transaction balances: per currency the balances sum to zero, credits equal debits, held amounts sum to zero, and a transaction moves one currency between two balances
-- Rule 1.1 (RESULTS.md). A Blnk transaction has one source and one destination, so the
-- ledger balances when every currency's balances sum to zero.
SELECT 'balances do not sum to zero' AS what, currency AS item, sum(balance)::text AS detail
FROM blnk.balances GROUP BY currency HAVING sum(balance) <> 0
UNION ALL
SELECT 'credits differ from debits', currency, (sum(credit_balance) - sum(debit_balance))::text
FROM blnk.balances GROUP BY currency HAVING sum(credit_balance) <> sum(debit_balance)
UNION ALL
SELECT 'held amounts do not sum to zero', currency, sum(inflight_balance)::text
FROM blnk.balances GROUP BY currency
HAVING sum(inflight_balance) <> 0 OR sum(inflight_credit_balance) <> sum(inflight_debit_balance)
UNION ALL
SELECT 'balance is not credit minus debit', coalesce(indicator, balance_id), balance::text
FROM blnk.balances
WHERE balance <> credit_balance - debit_balance
   OR inflight_balance <> inflight_credit_balance - inflight_debit_balance
UNION ALL
SELECT 'moves another currency or to itself', t.reference, t.currency
FROM blnk.transactions t
JOIN blnk.balances s ON s.balance_id = t.source
JOIN blnk.balances d ON d.balance_id = t.destination
WHERE t.status IN ('APPLIED', 'INFLIGHT', 'VOID')
  AND (s.currency <> t.currency OR d.currency <> t.currency OR t.source = t.destination)
