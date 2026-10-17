//! 锁定测试：备份相关配置项与 `ntp_servers` 的默认值与非法值。
//! 规则见 AGENTS.md「备份」、docs/domain.md「时间」门店节点校时，接口见 docs/interfaces.md「boh-server：配置」。

use std::path::Path;

use boh_domain::time::parse_closing_backup_time;
use boh_server::config::{NtpServer, parse_config};

const BASE: &str = r#"
store_id = "01890a5d-ac96-774b-bcce-b302099a8050"
db_path = "/var/lib/boh/boh.db"
listen_addr = "127.0.0.1:8080"
timezone = "Asia/Shanghai"
business_day_cutoff = "04:00"
ntp_servers = ["ntp.aliyun.com"]
"#;

/// 不含 `ntp_servers`、含 `backup_dir` 的配置。
const WITHOUT_NTP: &str = r#"
store_id = "01890a5d-ac96-774b-bcce-b302099a8050"
db_path = "/var/lib/boh/boh.db"
listen_addr = "127.0.0.1:8080"
timezone = "Asia/Shanghai"
business_day_cutoff = "04:00"
backup_dir = "/var/lib/boh/backups"
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

fn ntp(value: &str) -> Result<Vec<NtpServer>, ()> {
    parse_config(&format!("{WITHOUT_NTP}ntp_servers = {value}\n"))
        .map(|config| config.ntp_servers)
        .map_err(|_| ())
}

fn server(host: &str, port: u16) -> NtpServer {
    NtpServer {
        host: host.to_owned(),
        port,
    }
}

// 「ntp_servers：必填，非空字符串数组，每项为 host 或 host:port（端口缺省 123，IPv6 地址写在方括号中）」：
// 按原顺序保存；host 中的 IPv6 地址不带方括号。
#[test]
fn ntp_servers_keep_order_and_default_to_port_123() {
    assert_eq!(
        ntp(r#"["ntp.aliyun.com", "ntp.tencent.com:1123", "127.0.0.1", "192.168.1.1:123"]"#),
        Ok(vec![
            server("ntp.aliyun.com", 123),
            server("ntp.tencent.com", 1123),
            server("127.0.0.1", 123),
            server("192.168.1.1", 123),
        ])
    );
    assert_eq!(
        ntp(r#"["[::1]", "[2001:db8::1]:5123", "[fe80::1]:65535", "a:1"]"#),
        Ok(vec![
            server("::1", 123),
            server("2001:db8::1", 5123),
            server("fe80::1", 65535),
            server("a", 1),
        ])
    );
}

// 「缺失、为空或某项无法解析时拒绝启动」。
#[test]
fn ntp_servers_must_be_a_non_empty_array_of_valid_entries() {
    assert!(parse_config(WITHOUT_NTP).is_err(), "missing");
    for value in [
        "[]",
        r#""ntp.aliyun.com""#,
        "[123]",
        r#"["ntp.aliyun.com", 123]"#,
        r#"[""]"#,
        r#"["ntp.aliyun.com:"]"#,
        r#"[":123"]"#,
        r#"["ntp.aliyun.com:abc"]"#,
        r#"["ntp.aliyun.com:0"]"#,
        r#"["ntp.aliyun.com:65536"]"#,
        r#"["ntp.aliyun.com:-1"]"#,
        r#"["ntp.aliyun.com:123:456"]"#,
        r#"["::1"]"#,
        r#"["2001:db8::1"]"#,
        r#"["[::1"]"#,
        r#"["[::1]:"]"#,
        r#"["[::1]x"]"#,
        r#"["[]"]"#,
        r#"["[]:123"]"#,
        r#"["ntp aliyun.com"]"#,
        r#"[" ntp.aliyun.com"]"#,
        r#"["ntp.aliyun.com "]"#,
        r#"["ntp.aliyun.com", ""]"#,
    ] {
        assert_eq!(ntp(value), Err(()), "{value}");
    }
}
