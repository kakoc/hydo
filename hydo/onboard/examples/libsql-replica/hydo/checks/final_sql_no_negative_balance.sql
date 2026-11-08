-- hydo: store=primary
-- hydo-msg: no account's balance is negative
-- A SQL check: the query must return no rows (MANIFEST.md, "SQL checks").
SELECT id, bal FROM hydo_accounts WHERE bal < 0
