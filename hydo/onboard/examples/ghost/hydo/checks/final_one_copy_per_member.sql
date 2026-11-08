-- hydo: store=db
-- hydo-msg: a member is a recipient of one newsletter twice
SELECT email_id, member_email, count(*) AS copies FROM email_recipients GROUP BY email_id, member_email HAVING count(*) > 1
