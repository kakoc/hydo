-- hydo-msg: an atomic bulk is applied whole or not at all: both of its transactions are in the ledger, or neither
-- Rule 1g (RESULTS.md). The client sends two transfers per bulk with ?atomic=true and tags them
-- hydo_part=1 and 2.
SELECT metadata ->> 'hydo_op' AS bulk, string_agg(metadata ->> 'hydo_part', ',' ORDER BY metadata ->> 'hydo_part') AS parts
FROM _default.transactions
WHERE ledger = 'main' AND metadata ? 'hydo_part'
GROUP BY 1
HAVING count(DISTINCT metadata ->> 'hydo_part') <> 2
