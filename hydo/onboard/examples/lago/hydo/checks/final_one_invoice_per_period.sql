-- hydo-msg: a subscription is never invoiced twice for one billing period
SELECT s.external_id, i.from_datetime, i.to_datetime, count(*) AS invoices
FROM invoice_subscriptions i
JOIN invoices v ON v.id = i.invoice_id
JOIN subscriptions s ON s.id = i.subscription_id
WHERE i.recurring IS TRUE
  AND v.status NOT IN (2, 4, 8)   -- voided, failed, deleted
GROUP BY s.external_id, i.from_datetime, i.to_datetime
HAVING count(*) > 1
