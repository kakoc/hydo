-- hydo: store=db
-- hydo-msg: A voucher code is never used more times than its voucher's usage limit.
-- evidence: saleor/graphql/schema.graphql:31833: Limit number of times this voucher can be used in total.
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT c.code, c.used, v.usage_limit FROM discount_vouchercode c JOIN discount_voucher v ON v.id = c.voucher_id WHERE v.usage_limit IS NOT NULL AND c.used > v.usage_limit
