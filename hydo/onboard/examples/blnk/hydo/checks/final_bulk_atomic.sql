-- hydo-msg: an atomic bulk applies whole or not at all: both of its transfers stay applied, or neither does (a rolled-back transfer is refunded)
-- Rule 1.7 (RESULTS.md). The client's bulks are the references k<n>.1 and k<n>.2. A transfer's
-- effect is its APPLIED copy less the APPLIED refunds of that copy. A bulk whose second transfer
-- was never saved holds when its first has no effect.
WITH items AS (
  SELECT split_part(p.reference, '.', 1) AS bulk, p.reference, p.precise_amount AS amount,
         CASE WHEN q.status = 'APPLIED' THEN q.precise_amount ELSE 0 END
         - coalesce((SELECT sum(r.precise_amount) FROM blnk.transactions r
                     WHERE q.transaction_id IS NOT NULL AND r.parent_transaction = q.transaction_id
                       AND r.status = 'APPLIED'), 0) AS effect
  FROM blnk.transactions p
  LEFT JOIN blnk.transactions q ON q.reference = p.reference || '_q'
  WHERE p.reference ~ '^k[0-9]+\.[12]$' AND p.status = 'QUEUED')
SELECT bulk, count(*)::text AS saved,
       string_agg(reference || ' ' || effect::text || ' of ' || amount::text, ', ' ORDER BY reference) AS effects
FROM items
GROUP BY bulk
HAVING NOT (bool_and(effect = 0) OR (count(*) = 2 AND bool_and(effect = amount)))
