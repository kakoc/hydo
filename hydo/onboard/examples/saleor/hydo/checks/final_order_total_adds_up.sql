-- hydo: store=db
-- hydo-msg: An order's total is its lines' total plus its shipping price.
-- evidence: saleor/order/base_calculations.py:57: return subtotal + shipping_price
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT o.number, o.total_gross_amount, o.shipping_price_gross_amount, sum(l.total_price_gross_amount) AS lines
FROM order_order o JOIN order_orderline l ON l.order_id = o.id
WHERE o.status <> 'draft'
GROUP BY o.id, o.number, o.total_gross_amount, o.shipping_price_gross_amount, o.subtotal_gross_amount
HAVING o.total_gross_amount <> sum(l.total_price_gross_amount) + o.shipping_price_gross_amount
    OR o.subtotal_gross_amount <> sum(l.total_price_gross_amount)
