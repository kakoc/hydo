-- hydo-msg: a revert with atEffectiveDate=true has the original's transaction time; one without it has the time the original was reverted at
-- Rule 1f (RESULTS.md), the reverting page of the documentation. The client tags each revert
-- with hydo_at=1 or 0 (the atEffectiveDate it sent).
SELECT rv.id::text AS revert, o.id::text AS original,
       'hydo_at=' || (rv.metadata ->> 'hydo_at') || ' revert at ' || rv.timestamp::text || ', original at ' || o.timestamp::text || ', reverted_at ' || coalesce(o.reverted_at::text, 'null') AS detail
FROM _default.transactions rv
JOIN _default.transactions o ON o.ledger = rv.ledger AND o.id = (rv.metadata ->> 'com.formance.spec/state/reverts')::numeric
WHERE rv.ledger = 'main' AND rv.metadata ? 'hydo_at'
  AND ((rv.metadata ->> 'hydo_at' = '1' AND rv.timestamp <> o.timestamp)
    OR (rv.metadata ->> 'hydo_at' = '0' AND rv.timestamp IS DISTINCT FROM o.reverted_at))
