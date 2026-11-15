-- hydo: store=db
-- hydo-msg: Every order placed from a checkout was charged its total (the channel allows no unpaid orders, and orders are confirmed at once, which charges gift cards).
-- evidence: saleor/graphql/schema.graphql:9387: Determine if it is possible to place unpaid order by calling `checkoutComplete` mutation.
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT o.number, o.total_gross_amount,
       coalesce(sum(CASE WHEN e.type = 'charge_success' THEN e.amount_value ELSE 0 END), 0) AS charged
FROM order_order o
LEFT JOIN payment_transactionitem t ON t.order_id = o.id
LEFT JOIN payment_transactionevent e ON e.transaction_id = t.id
WHERE o.origin = 'checkout' AND o.status NOT IN ('draft', 'canceled', 'expired')
GROUP BY o.id, o.number, o.total_gross_amount
HAVING coalesce(sum(CASE WHEN e.type = 'charge_success' THEN e.amount_value ELSE 0 END), 0) < o.total_gross_amount
