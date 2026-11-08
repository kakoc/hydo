-- hydo: store=db
-- hydo-msg: a newsletter marked sent counts other recipients than its batches hold
SELECT e.id, e.email_count, count(r.id) AS recipients
FROM emails e LEFT JOIN email_recipients r ON r.email_id = e.id
WHERE e.status = 'submitted'
GROUP BY e.id, e.email_count
HAVING e.email_count <> count(r.id)
