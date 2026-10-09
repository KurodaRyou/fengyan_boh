-- 007_lot_numbers.sql
-- 批次以批次号 lot_id（<类型>-<编码>-<YYYYMMDD>-<流水号>）为主键，拆出批次日期与流水号，FIFO 按（批次日期, 流水号）；
-- 余量可以为负；盘点不新建批次，origin 只留 RECEIPT / PRODUCTION。见 docs/domain.md「批次」「投影表」。
-- 合并到 main 后禁止修改，变更请新增下一个编号的文件。
--
-- UUID 批次标识换算不出批次号（GOODS_RECEIVED@1 的 payload 中没有类型和编码），账本中有 @1 收货时拒绝迁移；
-- 物料编码是批次号的一段，items 中有不合规的 code 时同样拒绝。批次只由收货建立，所以通过检查时
-- inventory_lots 与 inventory_movements 都没有行，直接重建，不搬数据。

-- 前置检查：不满足时 CHECK 失败，错误信息带约束名，整个迁移事务回滚。
CREATE TEMP TABLE migration_007_check (
    goods_received_v1  INTEGER NOT NULL
                       CONSTRAINT "ledger has GOODS_RECEIVED@1 events; reinitialize the database"
                       CHECK (goods_received_v1 = 0),
    invalid_item_codes INTEGER NOT NULL
                       CONSTRAINT "items.code must contain only A-Z, 0-9 and _"
                       CHECK (invalid_item_codes = 0)
) STRICT;

INSERT INTO migration_007_check (goods_received_v1, invalid_item_codes)
SELECT (SELECT count(*) FROM store_events WHERE event_type = 'GOODS_RECEIVED' AND schema_version = 1),
       (SELECT count(*) FROM items WHERE code = '' OR code GLOB '*[^A-Z0-9_]*');

DROP TABLE migration_007_check;

DROP VIEW inventory_on_hand;
DROP TABLE inventory_movements;
DROP TABLE inventory_lots;

-- 批次：每条收货行、每次生产产出一行；余量为 0 的批次保留，余量可以为负。
-- lot_date 是批次日期（'YYYY-MM-DD'），lot_serial 是流水号，两者都从 lot_id 拆出，写入后不变。
-- source_event_seq、source_line_no 是建批次的事件和原 payload 下标，只用于追溯。
CREATE TABLE inventory_lots (
    lot_id              TEXT PRIMARY KEY
                        CHECK (lot_id GLOB 'RAW-*' OR lot_id GLOB 'SEMI-*' OR lot_id GLOB 'FINISHED-*'),
    item_id             TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    lot_date            TEXT NOT NULL CHECK (date(lot_date) IS lot_date),
    lot_serial          INTEGER NOT NULL CHECK (lot_serial BETWEEN 1 AND 999),
    origin              TEXT NOT NULL CHECK (origin IN ('RECEIPT', 'PRODUCTION')),
    source_event_seq    INTEGER NOT NULL REFERENCES store_events(seq),  -- 建批次的事件
    source_line_no      INTEGER NOT NULL CHECK (source_line_no >= 0),    -- 原 payload 下标；生产 output 为 0
    remaining_qty       INTEGER NOT NULL,
    expires_at          INTEGER,                                         -- NULL：payload 中省略
    manufacturer_lot_no TEXT CHECK (manufacturer_lot_no IS NULL OR manufacturer_lot_no <> ''), -- NULL：payload 中省略
    UNIQUE (source_event_seq, source_line_no),
    -- 编码段：第一个 '-' 之后、末尾 13 个字符（'-YYYYMMDD-NNN'）之前，非空，只含 A-Z、0-9、_。
    CHECK (length(lot_id) - instr(lot_id, '-') - 13 >= 1
           AND substr(lot_id, instr(lot_id, '-') + 1, length(lot_id) - instr(lot_id, '-') - 13)
               NOT GLOB '*[^A-Z0-9_]*'),
    CHECK (substr(lot_id, -13) = '-' || replace(lot_date, '-', '') || '-' || printf('%03d', lot_serial))
) STRICT;

-- FIFO 与流水号分配都按 (item_id, lot_date, lot_serial) 查找；同一物料、同一日期内流水号唯一。
CREATE UNIQUE INDEX idx_inventory_lots_fifo ON inventory_lots(item_id, lot_date, lot_serial);

-- 库存流水：与 005 相同，lot_id 引用重建后的 inventory_lots。
CREATE TABLE inventory_movements (
    event_seq            INTEGER NOT NULL REFERENCES store_events(seq),
    movement_no          INTEGER NOT NULL CHECK (movement_no >= 0),
    item_id              TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    lot_id               TEXT REFERENCES inventory_lots(lot_id) DEFERRABLE INITIALLY DEFERRED,
    kind                 TEXT NOT NULL CHECK (kind IN ('RECEIPT', 'PRODUCE', 'CONSUME', 'WASTE', 'ADJUST')),
    alloc_source         TEXT NOT NULL CHECK (alloc_source IN
                         ('NEW_LOT', 'SPECIFIED', 'FIFO', 'SHORTFALL', 'COUNT', 'CORRECTION', 'REVERSAL', 'ABSORBED')),
    nominal_qty          INTEGER NOT NULL,
    qty_delta            INTEGER NOT NULL,
    absorbed_by_event_id TEXT REFERENCES store_events(id),           -- 直接取自 payload
    physical_at          INTEGER NOT NULL,                           -- 实物时点 t(e)
    business_date        TEXT NOT NULL CHECK (date(business_date) IS business_date),
    PRIMARY KEY (event_seq, movement_no),
    CHECK ((absorbed_by_event_id IS NULL AND alloc_source <> 'ABSORBED' AND qty_delta = nominal_qty)
        OR (absorbed_by_event_id IS NOT NULL AND alloc_source = 'ABSORBED' AND qty_delta = 0
            AND lot_id IS NULL)),
    CHECK (alloc_source NOT IN ('NEW_LOT', 'SPECIFIED', 'FIFO') OR lot_id IS NOT NULL),
    CHECK (alloc_source <> 'SHORTFALL' OR lot_id IS NULL)
) STRICT;

CREATE INDEX idx_inventory_movements_lot ON inventory_movements(lot_id);
CREATE INDEX idx_inventory_movements_item ON inventory_movements(item_id, business_date);

-- 账面数 = 批次余量之和 + 账外缺口；每个物料一行，没有库存的物料为 0。
CREATE VIEW inventory_on_hand (item_id, qty) AS
SELECT items.id,
       coalesce((SELECT sum(remaining_qty) FROM inventory_lots WHERE inventory_lots.item_id = items.id), 0)
     + coalesce((SELECT qty FROM inventory_unallocated WHERE inventory_unallocated.item_id = items.id), 0)
FROM items;
