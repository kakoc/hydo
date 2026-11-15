-- hydo: store=db
-- hydo-msg: A gift card's balance never goes below zero.
-- evidence: docs/adr/0004-race-safe-balance-adjustments.md:7: self-clamping so a card can never go negative
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT code, current_balance_amount FROM giftcard_giftcard WHERE current_balance_amount < 0
