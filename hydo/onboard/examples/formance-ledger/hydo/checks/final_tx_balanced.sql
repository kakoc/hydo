-- hydo-msg: every transaction balances: for each asset its moves sum to zero, and it has two moves (a debit and a credit) per posting
-- Rule 1a (RESULTS.md). PostgreSQL's tables of the ledger's default bucket, ledger "main".
SELECT 'unbalanced' AS what, transactions_id::text AS tx, asset AS detail
FROM _default.moves
WHERE ledger = 'main'
GROUP BY transactions_id, asset
HAVING sum(CASE WHEN is_source THEN -amount ELSE amount END) <> 0
UNION ALL
SELECT 'moves per posting', t.id::text, json_array_length(t.postings::json)::text || ' postings, ' || count(m.seq)::text || ' moves'
FROM _default.transactions t
LEFT JOIN _default.moves m ON m.ledger = t.ledger AND m.transactions_id = t.id
WHERE t.ledger = 'main'
GROUP BY t.id, t.postings
HAVING count(m.seq) <> 2 * json_array_length(t.postings::json)
