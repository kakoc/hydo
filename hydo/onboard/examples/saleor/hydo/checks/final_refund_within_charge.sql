-- hydo: store=db
-- hydo-msg: No payment is refunded for more than was charged by it.
-- evidence: saleor/graphql/payment/mutations/transaction/transaction_request_action.py:136: action_value = min(action_value, transaction.charged_value)
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT t.id, t.app_identifier,
       sum(CASE WHEN e.type = 'charge_success' THEN e.amount_value ELSE 0 END) AS charged,
       sum(CASE WHEN e.type = 'refund_success' THEN e.amount_value ELSE 0 END) AS refunded
FROM payment_transactionitem t JOIN payment_transactionevent e ON e.transaction_id = t.id
GROUP BY t.id, t.app_identifier
HAVING sum(CASE WHEN e.type = 'refund_success' THEN e.amount_value ELSE 0 END)
     > sum(CASE WHEN e.type = 'charge_success' THEN e.amount_value ELSE 0 END)
