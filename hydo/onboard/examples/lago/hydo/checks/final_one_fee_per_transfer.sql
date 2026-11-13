-- hydo-msg: an event of a charge paid in advance is charged at most once
SELECT pay_in_advance_event_transaction_id, count(*) AS fees, sum(units) AS units
FROM fees
WHERE pay_in_advance AND deleted_at IS NULL AND pay_in_advance_event_transaction_id IS NOT NULL
GROUP BY pay_in_advance_event_transaction_id
HAVING count(*) > 1
