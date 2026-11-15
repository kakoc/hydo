-- hydo: store=db
-- hydo-msg: A single-use code is spent (inactive) exactly when one order used it.
-- evidence: saleor/discount/utils/voucher.py:95: if voucher.single_use: deactivate_voucher_code(code)
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT c.code, c.is_active, (SELECT count(*) FROM order_order o WHERE o.voucher_code = c.code AND o.status NOT IN ('draft', 'canceled', 'expired')) AS orders
FROM discount_vouchercode c JOIN discount_voucher v ON v.id = c.voucher_id
WHERE v.single_use
  AND (CASE WHEN c.is_active THEN 0 ELSE 1 END)
      <> (SELECT count(*) FROM order_order o WHERE o.voucher_code = c.code AND o.status NOT IN ('draft', 'canceled', 'expired'))
