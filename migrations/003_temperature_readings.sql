-- 003_temperature_readings.sql
-- 温度记录投影。合并到 main 后禁止修改，变更请新增下一个编号的文件。
-- 只由 projections::apply 按 TEMPERATURE_LOGGED 写入，可由 rebuild-projections 从 store_events 完整重建。

-- 每条读数一行：id 是事件的 aggregate_id（TEMPERATURE_READING 聚合），event_seq 是该事件的 seq；
-- 其余列取自同一事件的列和 payload，不查主数据。
-- equipment_id 的外键延迟到提交时检查：重建在一个事务内清空并重写 equipment 与本表，顺序不限。
CREATE TABLE temperature_readings (
    id            TEXT PRIMARY KEY
                  CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    event_seq     INTEGER NOT NULL UNIQUE REFERENCES store_events(seq),
    equipment_id  TEXT NOT NULL REFERENCES equipment(id) DEFERRABLE INITIALLY DEFERRED,
    celsius_x10   INTEGER NOT NULL CHECK (celsius_x10 BETWEEN -500 AND 5000), -- 0.1 °C
    note          TEXT CHECK (note IS NULL OR (note <> '' AND length(note) <= 200)), -- NULL：payload 中省略
    actor_id      TEXT NOT NULL
                  CHECK (length(actor_id) = 36 AND actor_id = lower(actor_id) AND substr(actor_id, 15, 1) = '7'),
    device_id     TEXT NOT NULL
                  CHECK (length(device_id) = 36 AND device_id = lower(device_id) AND substr(device_id, 15, 1) = '7'),
    business_date TEXT NOT NULL CHECK (date(business_date) IS business_date),
    occurred_at   INTEGER NOT NULL,
    recorded_at   INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_temperature_readings_business_date
    ON temperature_readings(business_date, occurred_at, event_seq);
CREATE INDEX idx_temperature_readings_equipment
    ON temperature_readings(equipment_id, business_date);
