# Shared by the eventually_ command and the checks: what PostgreSQL holds, as JSON lines.
# dump_tx: every transaction with the indicators of its balances; dump_bal: every balance.
dump_tx() {
  hydo-sql read sql db "SELECT t.transaction_id, t.reference, t.status, coalesce(t.parent_transaction, '') AS parent_transaction, t.precise_amount::text AS precise_amount, s.indicator AS src, d.indicator AS dst FROM blnk.transactions t LEFT JOIN blnk.balances s ON s.balance_id = t.source LEFT JOIN blnk.balances d ON d.balance_id = t.destination"
}
dump_bal() {
  hydo-sql read sql db "SELECT indicator, credit_balance::text AS credit_balance, debit_balance::text AS debit_balance, inflight_credit_balance::text AS inflight_credit_balance, inflight_debit_balance::text AS inflight_debit_balance FROM blnk.balances WHERE indicator IS NOT NULL"
}
