-- hydo-msg: every account's volumes are the sum of its moves, and each move's post-commit volumes are the running sum of the account's moves in commit order
-- Rule 1b (RESULTS.md). accounts_volumes is what the API's balances read; moves is the history.
WITH m AS (
  SELECT accounts_address, asset,
         sum(CASE WHEN is_source THEN 0 ELSE amount END) AS input,
         sum(CASE WHEN is_source THEN amount ELSE 0 END) AS output
  FROM _default.moves WHERE ledger = 'main' GROUP BY 1, 2),
v AS (SELECT accounts_address, asset, input, output FROM _default.accounts_volumes WHERE ledger = 'main'),
run AS (
  SELECT seq, accounts_address, asset, post_commit_volumes AS pcv,
         sum(CASE WHEN is_source THEN 0 ELSE amount END) OVER w AS input,
         sum(CASE WHEN is_source THEN amount ELSE 0 END) OVER w AS output
  FROM _default.moves WHERE ledger = 'main'
  WINDOW w AS (PARTITION BY accounts_address, asset ORDER BY seq ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW))
SELECT 'balance' AS what, coalesce(v.accounts_address, m.accounts_address) || ' ' || coalesce(v.asset, m.asset) AS account,
       'volumes ' || coalesce(v.input, 0)::text || '/' || coalesce(v.output, 0)::text || ', moves ' || coalesce(m.input, 0)::text || '/' || coalesce(m.output, 0)::text AS detail
FROM v FULL JOIN m ON v.accounts_address = m.accounts_address AND v.asset = m.asset
WHERE coalesce(v.input, 0) <> coalesce(m.input, 0) OR coalesce(v.output, 0) <> coalesce(m.output, 0)
UNION ALL
SELECT 'post-commit volumes', accounts_address || ' ' || asset,
       'move ' || seq::text || ': ' || (pcv).inputs::text || '/' || (pcv).outputs::text || ', running sum ' || input::text || '/' || output::text
FROM run
WHERE pcv IS NULL OR (pcv).inputs <> input OR (pcv).outputs <> output
