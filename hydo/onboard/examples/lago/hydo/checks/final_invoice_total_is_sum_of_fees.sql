-- hydo-msg: an invoice's amounts add up: its fees amount is the sum of its fees, and its total is that minus coupons and credits plus taxes
SELECT v.id, v.status, v.invoice_type, v.fees_amount_cents, v.total_amount_cents,
       COALESCE(f.sum, 0) AS sum_of_fees
FROM invoices v
LEFT JOIN (SELECT invoice_id, SUM(amount_cents) AS sum FROM fees
           WHERE deleted_at IS NULL GROUP BY invoice_id) f ON f.invoice_id = v.id
WHERE v.status IN (1, 6)   -- finalized, closed
  AND (v.fees_amount_cents <> COALESCE(f.sum, 0)
       OR v.total_amount_cents <> v.fees_amount_cents - v.coupons_amount_cents
            - v.progressive_billing_credit_amount_cents + v.taxes_amount_cents
            - v.credit_notes_amount_cents - v.prepaid_credit_amount_cents)
