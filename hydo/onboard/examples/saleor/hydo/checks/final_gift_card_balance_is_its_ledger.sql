-- hydo: store=db
-- hydo-msg: A gift card's balance is what it was issued with, less what orders charged from it, plus what was refunded to it.
-- evidence: saleor/giftcard/gateway.py:251: def charge_gift_card ... Deduct authorized amount from the gift card balance
-- evidence: saleor/giftcard/gateway.py:443: gift_card.current_balance_amount = F("current_balance_amount") + amount
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT g.code, g.initial_balance_amount, g.current_balance_amount, coalesce(m.charged, 0) AS charged, coalesce(m.refunded, 0) AS refunded
FROM giftcard_giftcard g
LEFT JOIN (SELECT t.gift_card_id,
                  sum(CASE WHEN e.type = 'charge_success' THEN e.amount_value ELSE 0 END) AS charged,
                  sum(CASE WHEN e.type = 'refund_success' THEN e.amount_value ELSE 0 END) AS refunded
           FROM payment_transactionitem t JOIN payment_transactionevent e ON e.transaction_id = t.id
           WHERE t.gift_card_id IS NOT NULL GROUP BY t.gift_card_id) m ON m.gift_card_id = g.id
WHERE g.current_balance_amount <> g.initial_balance_amount - coalesce(m.charged, 0) + coalesce(m.refunded, 0)
