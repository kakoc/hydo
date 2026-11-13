-- hydo-msg: every invoice ends in a final state (finalized, voided or closed)
SELECT id, invoice_type, status, created_at
FROM invoices
WHERE status NOT IN (1, 2, 6)   -- finalized, voided, closed
