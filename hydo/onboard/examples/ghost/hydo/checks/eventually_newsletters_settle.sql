-- hydo: store=db within=180 after=heal poll=5
-- hydo-msg: a newsletter is still pending or submitting three minutes after the faults ended
SELECT id, status, email_count FROM emails WHERE status IN ('pending', 'submitting')
