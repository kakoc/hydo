-- hydo-msg: a reverted transaction has exactly one revert, only reverted transactions have one, and the revert moves the same amounts back
-- Rule 1f (RESULTS.md). A revert is a transaction with the metadata
-- com.formance.spec/state/reverts=<id>; the reverted one carries reverted_at.
WITH rv AS (
  SELECT id, (metadata ->> 'com.formance.spec/state/reverts')::numeric AS orig
  FROM _default.transactions
  WHERE ledger = 'main' AND metadata ? 'com.formance.spec/state/reverts'),
net AS (
  SELECT transactions_id, accounts_address, asset, sum(CASE WHEN is_source THEN -amount ELSE amount END) AS n
  FROM _default.moves WHERE ledger = 'main' GROUP BY 1, 2, 3)
SELECT 'revert count' AS what, t.id::text AS tx,
       count(rv.id)::text || ' reverts, reverted_at ' || coalesce(t.reverted_at::text, 'null') AS detail
FROM _default.transactions t LEFT JOIN rv ON rv.orig = t.id
WHERE t.ledger = 'main'
GROUP BY t.id, t.reverted_at
HAVING (t.reverted_at IS NULL) <> (count(rv.id) = 0) OR count(rv.id) > 1
UNION ALL
SELECT 'revert is not the mirror', rv.id::text, 'of ' || rv.orig::text || ': ' || x.accounts_address || ' ' || x.asset || ' off by ' || x.s::text
FROM rv CROSS JOIN LATERAL (
  SELECT accounts_address, asset, sum(n) AS s FROM net
  WHERE transactions_id = rv.id OR transactions_id = rv.orig
  GROUP BY 1, 2 HAVING sum(n) <> 0) x
