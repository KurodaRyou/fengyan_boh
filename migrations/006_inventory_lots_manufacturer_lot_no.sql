-- 006_inventory_lots_manufacturer_lot_no.sql
-- 批号是包装上由生产商印的批号，不是供应商的：inventory_lots.supplier_lot_no 改名为 manufacturer_lot_no。
-- RENAME COLUMN 同时改写列上的 CHECK 约束。合并到 main 后禁止修改，变更请新增下一个编号的文件。

ALTER TABLE inventory_lots RENAME COLUMN supplier_lot_no TO manufacturer_lot_no;
