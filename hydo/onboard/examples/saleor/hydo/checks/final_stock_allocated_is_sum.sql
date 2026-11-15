-- hydo: store=db
-- hydo-msg: A stock's allocated quantity is the sum of its allocations to order lines.
-- evidence: saleor/warehouse/models.py:557: Coalesce(Sum("stock__allocations__quantity_allocated"), 0)
-- proposed by rule suggestion (confidence medium); validated by hydo rules check
SELECT s.id, s.quantity_allocated, coalesce(sum(a.quantity_allocated), 0) FROM warehouse_stock s LEFT JOIN warehouse_allocation a ON a.stock_id = s.id GROUP BY s.id HAVING s.quantity_allocated <> coalesce(sum(a.quantity_allocated), 0)
