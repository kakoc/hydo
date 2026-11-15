-- hydo: store=db
-- hydo-msg: A checkout becomes at most one order.
-- evidence: saleor/order/models.py:50: Return non-draft order with matched checkout token.
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT checkout_token, count(*) FROM order_order WHERE checkout_token <> '' AND status <> 'draft' GROUP BY checkout_token HAVING count(*) > 1
