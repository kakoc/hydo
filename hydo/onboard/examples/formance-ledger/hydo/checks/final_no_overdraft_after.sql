-- hydo-msg: no account goes below what it may: users, shops and fees never below zero, a credit line never below -5000
-- Rule 1d (RESULTS.md), checked once more after the faults heal. Only world may go below
-- zero without a limit; the client draws on credit lines with "allowing overdraft up to [USD/2 5000]".
SELECT accounts_address, asset, (input - output)::text AS balance
FROM _default.accounts_volumes
WHERE ledger = 'main'
  AND ((accounts_address LIKE 'users:%' OR accounts_address LIKE 'shops:%' OR accounts_address = 'fees') AND input - output < 0
       OR accounts_address LIKE 'credit:%' AND input - output < -5000)
