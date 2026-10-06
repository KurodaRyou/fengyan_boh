-- 001_initial_schema.sql
-- 门店元数据、幂等记录、事件账本。合并到 main 后禁止修改，变更请新增下一个编号的文件。
-- UUIDv7 文本格式：36 位、小写、第 15 个字符（版本位）为 '7'。
-- 时间戳一律 Unix UTC 毫秒。

-- 门店身份（单行）。启动时与配置文件中的 store_id 比对，不一致拒绝启动。
CREATE TABLE store_meta (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    store_id   TEXT NOT NULL
               CHECK (length(store_id) = 36 AND store_id = lower(store_id) AND substr(store_id, 15, 1) = '7'),
    created_at INTEGER NOT NULL
) STRICT;

-- 已成功处理的命令（幂等键）。只记录成功结果；业务校验失败不落库。
CREATE TABLE processed_commands (
    command_id   TEXT PRIMARY KEY
                 CHECK (length(command_id) = 36 AND command_id = lower(command_id) AND substr(command_id, 15, 1) = '7'),
    command_type TEXT NOT NULL,                                   -- 例如 'waste.log'
    request      TEXT NOT NULL CHECK (json_type(request) = 'object'), -- 规范化请求，不含 command_id、sent_at
    response     TEXT NOT NULL CHECK (json_valid(response)),      -- 含 warnings 的完整 data
    recorded_at  INTEGER NOT NULL
) STRICT;

-- 业务事件账本，只追加。seq 是门店内唯一权威顺序（提交顺序），由 SQLite 分配，连续无空洞。
CREATE TABLE store_events (
    seq               INTEGER PRIMARY KEY,
    id                TEXT NOT NULL UNIQUE
                      CHECK (length(id) = 36 AND id = lower(id) AND substr(id, 15, 1) = '7'),
    event_type        TEXT NOT NULL,                              -- 例如 'WASTE_LOGGED'
    schema_version    INTEGER NOT NULL CHECK (schema_version >= 1),
    aggregate_type    TEXT NOT NULL,                              -- 例如 'WASTE_RECORD'
    aggregate_id      TEXT NOT NULL
                      CHECK (length(aggregate_id) = 36 AND aggregate_id = lower(aggregate_id) AND substr(aggregate_id, 15, 1) = '7'),
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version >= 1),
    command_id        TEXT NOT NULL
                      REFERENCES processed_commands(command_id) DEFERRABLE INITIALLY DEFERRED,
    actor_id          TEXT NOT NULL
                      CHECK (length(actor_id) = 36 AND actor_id = lower(actor_id) AND substr(actor_id, 15, 1) = '7'),
    device_id         TEXT NOT NULL,
    business_date     TEXT NOT NULL CHECK (date(business_date) IS business_date), -- 'YYYY-MM-DD'，且是真实日期
    occurred_at       INTEGER NOT NULL,                           -- 业务发生时间（校准后）
    recorded_at       INTEGER NOT NULL,                           -- 门店接收时间（系统时钟原值）
    payload           TEXT NOT NULL CHECK (json_type(payload) = 'object'),
    UNIQUE (aggregate_type, aggregate_id, aggregate_version)
) STRICT;

CREATE INDEX idx_store_events_business_date ON store_events(business_date);
CREATE INDEX idx_store_events_command ON store_events(command_id);

-- seq 必须连续：禁止显式指定跳号的 seq。
CREATE TRIGGER store_events_seq_contiguous AFTER INSERT ON store_events
WHEN NEW.seq <> 1 + coalesce((SELECT max(seq) FROM store_events WHERE seq < NEW.seq), 0)
BEGIN SELECT RAISE(ABORT, 'store_events.seq must be contiguous'); END;

-- 只追加：数据库层面禁止修改和删除（配合连接级 recursive_triggers = ON，REPLACE 也会被拦截）。
CREATE TRIGGER store_events_no_update BEFORE UPDATE ON store_events
BEGIN SELECT RAISE(ABORT, 'store_events is append-only'); END;

CREATE TRIGGER store_events_no_delete BEFORE DELETE ON store_events
BEGIN SELECT RAISE(ABORT, 'store_events is append-only'); END;

CREATE TRIGGER processed_commands_no_update BEFORE UPDATE ON processed_commands
BEGIN SELECT RAISE(ABORT, 'processed_commands is append-only'); END;

CREATE TRIGGER processed_commands_no_delete BEFORE DELETE ON processed_commands
BEGIN SELECT RAISE(ABORT, 'processed_commands is append-only'); END;

CREATE TRIGGER store_meta_no_update BEFORE UPDATE ON store_meta
BEGIN SELECT RAISE(ABORT, 'store_meta is immutable'); END;

CREATE TRIGGER store_meta_no_delete BEFORE DELETE ON store_meta
BEGIN SELECT RAISE(ABORT, 'store_meta is immutable'); END;
