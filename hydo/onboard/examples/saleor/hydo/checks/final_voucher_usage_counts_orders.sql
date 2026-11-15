-- hydo: store=db
-- hydo-msg: A limited voucher code's usage count equals the number of orders that used it.
-- evidence: saleor/discount/utils/voucher.py:99: def increase_voucher_code_usage_value ... Increase voucher code uses by 1.
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT c.code, c.used, (SELECT count(*) FROM order_order o WHERE o.voucher_code = c.code AND o.status NOT IN ('draft', 'canceled', 'expired')) AS orders
FROM discount_vouchercode c JOIN discount_voucher v ON v.id = c.voucher_id
WHERE v.usage_limit IS NOT NULL
  AND c.used <> (SELECT count(*) FROM order_order o WHERE o.voucher_code = c.code AND o.status NOT IN ('draft', 'canceled', 'expired'))
