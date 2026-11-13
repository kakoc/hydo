-- hydo-msg: the audit log is whole: every log's hash chains to the one before it, and every transaction has its log
-- Rule 1h (RESULTS.md). With the default feature HASH_LOGS=SYNC the ledger stores each log's hash
-- of the previous hash and the log; compute_hash is the ledger's own SQL function for it.
SELECT 'hash' AS what, l.id::text AS log, l.type::text AS detail
FROM _default.logs l
LEFT JOIN LATERAL (
  SELECT hash FROM _default.logs p WHERE p.ledger = l.ledger AND p.id < l.id ORDER BY p.id DESC LIMIT 1) p ON true
WHERE l.ledger = 'main' AND l.hash IS DISTINCT FROM _default.compute_hash(p.hash, l)
UNION ALL
SELECT 'transaction without a log', t.id::text, ''
FROM _default.transactions t
WHERE t.ledger = 'main' AND NOT EXISTS (
  SELECT 1 FROM _default.logs l
  WHERE l.ledger = t.ledger AND l.type IN ('NEW_TRANSACTION', 'REVERTED_TRANSACTION')
    AND (l.data -> 'transaction' ->> 'id')::numeric = t.id)
