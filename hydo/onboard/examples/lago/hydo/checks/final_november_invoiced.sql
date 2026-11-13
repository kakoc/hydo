-- hydo-msg: every subscription has its November usage on a finalized invoice (periodic, or the termination invoice)
SELECT s.external_id
FROM subscriptions s
WHERE NOT EXISTS (
  SELECT 1 FROM invoice_subscriptions i JOIN invoices v ON v.id = i.invoice_id
  WHERE i.subscription_id = s.id AND v.status = 1
    AND i.invoicing_reason IN ('subscription_periodic', 'subscription_terminating')   -- not the invoices of charges paid in advance
    AND i.charges_from_datetime < '2026-11-02' AND i.charges_to_datetime >= '2026-11-30')
