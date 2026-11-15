-- hydo: store=db
-- hydo-msg: A warehouse never allocates more of a variant than it has.
-- evidence: saleor/warehouse/management.py:198: raise InsufficientStock(insufficient_stock)
-- proposed by rule suggestion (confidence high); validated by hydo rules check
SELECT s.id, s.quantity, sum(a.quantity_allocated) FROM warehouse_stock s JOIN warehouse_allocation a ON a.stock_id = s.id GROUP BY s.id, s.quantity HAVING sum(a.quantity_allocated) > s.quantity
