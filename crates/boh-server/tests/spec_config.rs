//! 锁定测试：备份相关配置项的默认值与非法值。
//! 规则见 AGENTS.md「备份」，接口见 docs/interfaces.md「boh-server：配置」。

use std::path::Path;

use boh_domain::time::parse_closing_backup_time;
use boh_server::config::parse_config;

const BASE: &str = r#"
store_id = "01890a5d-ac96-774b-bcce-b302099a8050"
db_path = "/var/lib/boh/boh.db"
listen_addr = "127.0.0.1:8080"
timezone = "Asia/Shanghai"
business_day_cutoff = "04:00"
"#;

fn with(extra: &str) -> String {
    format!("{BASE}{extra}\n")
}

// 「备份目录由必填配置项 backup_dir 指定」；backup_keep_count 默认 168，closing_backup_time 默认 "23:30"。
#[test]
fn backup_dir_is_required_and_other_backup_settings_have_defaults() {
    assert!(parse_config(BASE).is_err());
    let config = parse_config(&with(r#"backup_dir = "/var/lib/boh/backups""#)).unwrap();
    assert_eq!(config.backup_dir, Path::new("/var/lib/boh/backups"));
    assert_eq!(config.backup_keep_count.get(), 168);
    assert_eq!(
        config.closing_backup_time,
        parse_closing_backup_time("23:30").unwrap()
    );
}

// 「backup_keep_count 必须是正整数，否则拒绝启动」：TOML 整数 1～u32::MAX。
#[test]
fn backup_keep_count_must_be_a_positive_integer() {
    for (value, expected) in [("1", 1), ("24", 24), ("4294967295", u32::MAX)] {
        let config = parse_config(&with(&format!(
            "backup_dir = \"/b\"\nbackup_keep_count = {value}"
        )))
        .unwrap();
        assert_eq!(config.backup_keep_count.get(), expected, "{value}");
    }
    for value in ["0", "-1", "1.5", "\"168\"", "4294967296", "true"] {
        assert!(
            parse_config(&with(&format!(
                "backup_dir = \"/b\"\nbackup_keep_count = {value}"
            )))
            .is_err(),
            "{value}"
        );
    }
}

// 「closing_backup_time：'HH:MM'，格式非法拒绝启动」。
#[test]
fn closing_backup_time_must_be_hh_mm() {
    for value in ["00:00", "21:45", "23:59"] {
        let config = parse_config(&with(&format!(
            "backup_dir = \"/b\"\nclosing_backup_time = \"{value}\""
        )))
        .unwrap();
        assert_eq!(
            config.closing_backup_time,
            parse_closing_backup_time(value).unwrap(),
            "{value}"
        );
    }
    for value in ["\"24:00\"", "\"9:30\"", "\"23:30:00\"", "\"\"", "2330"] {
        assert!(
            parse_config(&with(&format!(
                "backup_dir = \"/b\"\nclosing_backup_time = {value}"
            )))
            .is_err(),
            "{value}"
        );
    }
}
