-- hydo-msg: a hold never commits more than it held, is voided at most once, and its void releases exactly what was still held
-- Rule 1.4 (RESULTS.md). Blnk's commit-inflight page: "The total cannot exceed the original
-- inflight amount"; a void "releases whatever is still held" and "does not reverse amount you
-- already committed".
WITH h AS (
  SELECT t.reference, t.precise_amount AS held,
         coalesce(sum(c.precise_amount) FILTER (WHERE c.status = 'APPLIED'), 0) AS committed,
         coalesce(sum(c.precise_amount) FILTER (WHERE c.status = 'VOID'), 0) AS voided,
         count(c.id) FILTER (WHERE c.status = 'VOID') AS voids
  FROM blnk.transactions t
  LEFT JOIN blnk.transactions c ON c.parent_transaction = t.transaction_id
  WHERE t.status = 'INFLIGHT'
  GROUP BY t.reference, t.precise_amount)
SELECT reference, held::text,
       'committed ' || committed::text || ', voided ' || voided::text || ' in ' || voids::text || ' voids' AS detail
FROM h
WHERE committed + voided > held OR voids > 1 OR (voids = 1 AND committed + voided <> held)
