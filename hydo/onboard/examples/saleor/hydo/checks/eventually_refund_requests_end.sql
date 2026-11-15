-- hydo: store=db within=400 after=commands
-- hydo-msg: Every refund that staff request ends, as a success or a failure, after the faults stop (Saleor sends it to the payment app from a Celery task and retries it 5 times).
-- evidence: saleor/webhook/transport/synchronous/transport.py:69: @app.task(bind=True, retry_backoff=10, retry_kwargs={"max_retries": 5}) def handle_transaction_request_task
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT t.id, t.app_identifier,
       count(*) FILTER (WHERE e.type = 'refund_request') AS requests,
       count(*) FILTER (WHERE e.type IN ('refund_success', 'refund_failure')) AS ends
FROM payment_transactionitem t JOIN payment_transactionevent e ON e.transaction_id = t.id
GROUP BY t.id, t.app_identifier
HAVING count(*) FILTER (WHERE e.type = 'refund_request') > count(*) FILTER (WHERE e.type IN ('refund_success', 'refund_failure'))
