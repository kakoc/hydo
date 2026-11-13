-- hydo-msg: each move's post-commit effective volumes are the sum of the account's moves up to its effective date (backdated transactions included)
-- Rule 1c (RESULTS.md). The ledger keeps these with two triggers on moves (set_effective_volumes,
-- update_effective_volumes); a transaction dated before others changes the later moves' values.
WITH run AS (
  SELECT seq, accounts_address, asset, effective_date, post_commit_effective_volumes AS pcev,
         sum(CASE WHEN is_source THEN 0 ELSE amount END) OVER w AS input,
         sum(CASE WHEN is_source THEN amount ELSE 0 END) OVER w AS output
  FROM _default.moves WHERE ledger = 'main'
  WINDOW w AS (PARTITION BY accounts_address, asset ORDER BY effective_date, seq ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW))
SELECT 'effective volumes' AS what, accounts_address || ' ' || asset AS account,
       'move ' || seq::text || ' at ' || effective_date::text || ': ' || coalesce((pcev).inputs::text, 'null') || '/' || coalesce((pcev).outputs::text, 'null') || ', sum ' || input::text || '/' || output::text AS detail
FROM run
WHERE pcev IS NULL OR (pcev).inputs <> input OR (pcev).outputs <> output
