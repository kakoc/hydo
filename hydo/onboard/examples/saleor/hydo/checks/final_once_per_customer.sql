-- hydo: store=db
-- hydo-msg: A customer uses a once-per-customer voucher in at most one order.
-- evidence: saleor/graphql/schema.graphql:11410: Determine if the voucher usage should be limited to one use per customer.
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT o.voucher_code, o.user_email, count(*) FROM order_order o
JOIN discount_vouchercode c ON c.code = o.voucher_code JOIN discount_voucher v ON v.id = c.voucher_id
WHERE v.apply_once_per_customer AND o.status NOT IN ('draft', 'canceled', 'expired')
GROUP BY o.voucher_code, o.user_email HAVING count(*) > 1
