-- hydo: store=db
-- hydo-msg: A gift card never holds more than it was issued with (this shop never tops a card up; a refund only gives back what was charged from it).
-- evidence: saleor/giftcard/gateway.py:408: def refund_gift_card_transaction ... Refund funds to a gift card which previously were charged from the same gift card.
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT code, initial_balance_amount, current_balance_amount FROM giftcard_giftcard WHERE current_balance_amount > initial_balance_amount
