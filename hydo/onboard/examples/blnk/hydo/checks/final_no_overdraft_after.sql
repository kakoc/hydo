-- hydo-msg: after the faults: users, shops, the merchant and the empty account are not below zero, also counting what they hold; a credit line is not below its limit of 50.00
-- Rule 1.6 (RESULTS.md). Blnk's overdrafts page: without allow_overdraft "the transaction is
-- rejected"; with overdraft_limit it "succeeds only if the resulting negative balance stays
-- within the limit". Blnk counts held amounts in the funds a transfer may use.
SELECT indicator, balance::text, inflight_debit_balance::text AS held
FROM blnk.balances
WHERE (indicator IN ('@u1', '@u2', '@u3', '@u4', '@s1', '@s2', '@m', '@e')
       AND (balance < 0 OR balance - inflight_debit_balance < 0))
   OR (indicator IN ('@c1', '@c2') AND balance - inflight_debit_balance < -5000)
