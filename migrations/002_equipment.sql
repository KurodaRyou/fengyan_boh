-- 002_equipment.sql
-- 设备主数据投影。合并到 main 后禁止修改，变更请新增下一个编号的文件。
-- 只由 projections::apply 按 MASTER_DATA_CHANGED（entity = 'EQUIPMENT'）写入，可由 rebuild-projections 从 store_events 完整重建。

-- 每台设备一行：id 是事件的 aggregate_id，revision 是 aggregate_version，其余列是快照。
CREATE TABLE equipment (
    id             TEXT PRIMARY KEY
                   CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    code           TEXT NOT NULL UNIQUE CHECK (code <> ''),
    name           TEXT NOT NULL CHECK (name <> ''),
    equipment_type TEXT NOT NULL
                   CHECK (equipment_type IN ('FRIDGE', 'FREEZER', 'BLAST_FREEZER', 'OVEN', 'PROOFER', 'MIXER', 'OTHER')),
    active         INTEGER NOT NULL CHECK (active IN (0, 1)),
    revision       INTEGER NOT NULL CHECK (revision >= 1)
) STRICT;
