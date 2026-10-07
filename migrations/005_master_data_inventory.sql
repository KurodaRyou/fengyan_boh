-- 005_master_data_inventory.sql
-- 其余主数据（物料、配方、供应商、报损原因）与库存投影。合并到 main 后禁止修改，变更请新增下一个编号的文件。
-- 全部是投影：只由 projections::apply 按事件写入，可由 rebuild-projections 从 store_events 完整重建。
-- 引用其他投影的外键延迟到提交时检查：重建在一个事务内清空并重写全部投影，顺序不限。
-- 主数据表每行的 id 是事件的 aggregate_id，revision 是 aggregate_version，其余列取自快照。

-- 物料（MASTER_DATA_CHANGED，entity = 'ITEM'）。快照的 units 拆到 item_units。
CREATE TABLE items (
    id                    TEXT PRIMARY KEY
                          CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    code                  TEXT NOT NULL UNIQUE CHECK (code <> ''),
    name                  TEXT NOT NULL CHECK (name <> ''),
    base_unit             TEXT NOT NULL CHECK (base_unit IN ('g', 'ml', 'pcs')),
    category              TEXT NOT NULL CHECK (category IN ('RAW', 'SEMI', 'FINISHED')),
    default_shelf_life_ms INTEGER CHECK (default_shelf_life_ms IS NULL OR default_shelf_life_ms > 0), -- NULL：快照中省略
    active                INTEGER NOT NULL CHECK (active IN (0, 1)),
    revision              INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

-- 物料的非基本单位：快照 units 的每一项一行，不含基本单位。
CREATE TABLE item_units (
    item_id           TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    unit_code         TEXT NOT NULL CHECK (unit_code <> ''),
    base_qty_per_unit INTEGER NOT NULL CHECK (base_qty_per_unit > 0),
    PRIMARY KEY (item_id, unit_code)
) STRICT;

-- 配方（MASTER_DATA_CHANGED，entity = 'RECIPE'）。快照的 versions 拆到 recipe_versions 与 recipe_lines。
CREATE TABLE recipes (
    id             TEXT PRIMARY KEY
                   CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    code           TEXT NOT NULL UNIQUE CHECK (code <> ''),
    name           TEXT NOT NULL CHECK (name <> ''),
    output_item_id TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    active         INTEGER NOT NULL CHECK (active IN (0, 1)),
    revision       INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

CREATE TABLE recipe_versions (
    recipe_id            TEXT NOT NULL REFERENCES recipes(id) DEFERRABLE INITIALLY DEFERRED,
    version              INTEGER NOT NULL CHECK (version >= 1),
    output_qty_per_batch INTEGER NOT NULL CHECK (output_qty_per_batch > 0), -- 产出物料的基本单位
    PRIMARY KEY (recipe_id, version)
) STRICT;

-- 每个版本的用料：line_no 是该版本 lines 数组的下标，从 0 开始。
CREATE TABLE recipe_lines (
    recipe_id     TEXT NOT NULL,
    version       INTEGER NOT NULL,
    line_no       INTEGER NOT NULL CHECK (line_no >= 0),
    item_id       TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    qty_per_batch INTEGER NOT NULL CHECK (qty_per_batch > 0), -- 该物料的基本单位
    PRIMARY KEY (recipe_id, version, line_no),
    UNIQUE (recipe_id, version, item_id),
    FOREIGN KEY (recipe_id, version) REFERENCES recipe_versions(recipe_id, version)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- 供应商（MASTER_DATA_CHANGED，entity = 'SUPPLIER'）。
CREATE TABLE suppliers (
    id            TEXT PRIMARY KEY
                  CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    code          TEXT NOT NULL UNIQUE CHECK (code <> ''),
    name          TEXT NOT NULL CHECK (name <> ''),
    contact_phone TEXT CHECK (contact_phone IS NULL OR contact_phone <> ''), -- NULL：快照中省略
    active        INTEGER NOT NULL CHECK (active IN (0, 1)),
    revision      INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

-- 报损原因（MASTER_DATA_CHANGED，entity = 'WASTE_REASON'）。WASTE_LOGGED 的 reason_code 对应 code。
CREATE TABLE waste_reasons (
    id       TEXT PRIMARY KEY
             CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    code     TEXT NOT NULL UNIQUE CHECK (code <> ''),
    name     TEXT NOT NULL CHECK (name <> ''),
    active   INTEGER NOT NULL CHECK (active IN (0, 1)),
    revision INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;

-- 批次：每条收货行、每次生产产出、每个盘盈一行；余量为 0 的批次保留。
-- FIFO 按（盘盈优先、source_seq、source_line_no）升序，见 docs/domain.md「批次」。
CREATE TABLE inventory_lots (
    lot_id          TEXT PRIMARY KEY
                    CHECK (length(lot_id) = 36 AND lot_id = lower(lot_id) AND substr(lot_id, 15, 1) = '7'),
    item_id         TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    origin          TEXT NOT NULL CHECK (origin IN ('RECEIPT', 'PRODUCTION', 'COUNT_GAIN')),
    source_seq      INTEGER NOT NULL REFERENCES store_events(seq),  -- 建批次的事件
    source_line_no  INTEGER NOT NULL CHECK (source_line_no >= 0),    -- 原 payload 下标；生产 output 为 0
    remaining_qty   INTEGER NOT NULL CHECK (remaining_qty >= 0),
    expires_at      INTEGER,                                         -- NULL：payload 中省略
    supplier_lot_no TEXT CHECK (supplier_lot_no IS NULL OR supplier_lot_no <> ''), -- NULL：payload 中省略
    UNIQUE (source_seq, source_line_no)
) STRICT;

CREATE INDEX idx_inventory_lots_item ON inventory_lots(item_id);

-- 账外缺口：每个物料至多一行。
CREATE TABLE inventory_unallocated (
    item_id TEXT PRIMARY KEY REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    qty     INTEGER NOT NULL CHECK (qty <= 0)
) STRICT;

-- 库存流水：每条库存影响按批次展开，一行一条。数量带符号，增加为正。
-- nominal_qty 是按申报内容应有的变动量，qty_delta 是实际作用于账面的变动量。
-- lot_id 为 NULL：被吸收，或作用于账外缺口。
CREATE TABLE inventory_movements (
    event_seq            INTEGER NOT NULL REFERENCES store_events(seq),
    line_no              INTEGER NOT NULL CHECK (line_no >= 0),
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
    PRIMARY KEY (event_seq, line_no),
    CHECK ((absorbed_by_event_id IS NULL AND alloc_source <> 'ABSORBED' AND qty_delta = nominal_qty)
        OR (absorbed_by_event_id IS NOT NULL AND alloc_source = 'ABSORBED' AND qty_delta = 0
            AND lot_id IS NULL)),
    CHECK (alloc_source NOT IN ('NEW_LOT', 'SPECIFIED', 'FIFO') OR lot_id IS NOT NULL),
    CHECK (alloc_source <> 'SHORTFALL' OR lot_id IS NULL)
) STRICT;

CREATE INDEX idx_inventory_movements_lot ON inventory_movements(lot_id);
CREATE INDEX idx_inventory_movements_item ON inventory_movements(item_id, business_date);

-- 盘点：每次盘点（STOCK_ADJUSTED 事件）的每个被盘物料一行，零差异也写。
-- observed_at 是盘点事件的 occurred_at；吸收判定按 (item_id, observed_at, event_seq) 查找。
CREATE TABLE inventory_counts (
    event_seq   INTEGER NOT NULL REFERENCES store_events(seq),
    item_id     TEXT NOT NULL REFERENCES items(id) DEFERRABLE INITIALLY DEFERRED,
    observed_at INTEGER NOT NULL,
    book_qty    INTEGER NOT NULL,                     -- 提交时范围内批次余量与账外缺口之和
    counted_qty INTEGER NOT NULL CHECK (counted_qty >= 0),
    PRIMARY KEY (event_seq, item_id)
) STRICT;

CREATE INDEX idx_inventory_counts_item ON inventory_counts(item_id, observed_at, event_seq);

-- 账面数 = 批次余量之和 + 账外缺口；每个物料一行，没有库存的物料为 0。
CREATE VIEW inventory_on_hand (item_id, qty) AS
SELECT items.id,
       coalesce((SELECT sum(remaining_qty) FROM inventory_lots WHERE inventory_lots.item_id = items.id), 0)
     + coalesce((SELECT qty FROM inventory_unallocated WHERE inventory_unallocated.item_id = items.id), 0)
FROM items;
