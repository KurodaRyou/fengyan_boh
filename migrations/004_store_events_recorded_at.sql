-- 004_store_events_recorded_at.sql
-- /health 的 clock_regression_ms 每次请求都计算 max(store_events.recorded_at)；索引让它不必扫描整个账本。
-- 合并到 main 后禁止修改，变更请新增下一个编号的文件。

CREATE INDEX idx_store_events_recorded_at ON store_events(recorded_at);
