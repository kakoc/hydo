-- hydo-msg: a transaction with a reference is recorded once: no client operation is saved, applied or rejected twice, also when the client retried after a lost answer
-- Rule 1.5 (RESULTS.md). The client gives every transaction its reference as its description,
-- and Blnk copies the description to the transaction's queued copy and its outcomes. A refund
-- (an atomic bulk's rollback) and the commits of a hold carry it too, so they are left out.
SELECT description AS operation, status, count(*)::text AS rows
FROM blnk.transactions
WHERE description ~ '^[dtscpok][0-9]+(\.[12])?$'
  AND status IN ('QUEUED', 'APPLIED', 'REJECTED')
  AND reference NOT LIKE '%\_refund'
GROUP BY description, status
HAVING count(*) > 1
UNION ALL
SELECT description, 'APPLIED and REJECTED', count(*)::text
FROM blnk.transactions
WHERE description ~ '^[dtscpok][0-9]+(\.[12])?$' AND status IN ('APPLIED', 'REJECTED')
  AND reference NOT LIKE '%\_refund'
GROUP BY description
HAVING count(DISTINCT status) > 1
