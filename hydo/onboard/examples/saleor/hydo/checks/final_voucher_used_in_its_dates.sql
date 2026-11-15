-- hydo: store=db
-- hydo-msg: No order is placed with a voucher after the voucher ended.
-- evidence: saleor/discount/models.py:84: lookup = (Q(end_date__isnull=True) | Q(end_date__gte=date))
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT o.number, o.voucher_code, o.created_at, v.end_date FROM order_order o
JOIN discount_vouchercode c ON c.code = o.voucher_code JOIN discount_voucher v ON v.id = c.voucher_id
WHERE v.end_date IS NOT NULL AND o.created_at > v.end_date AND o.status <> 'draft'
