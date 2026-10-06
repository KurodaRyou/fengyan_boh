use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use boh_domain::StoreId;
use boh_domain::time::{
    BusinessDayCutoff, StoreTimeZone, parse_business_day_cutoff, parse_timezone,
};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub store_id: StoreId,
    pub db_path: PathBuf,
    pub listen_addr: SocketAddr,
    #[serde(deserialize_with = "timezone")]
    pub timezone: StoreTimeZone,
    #[serde(deserialize_with = "business_day_cutoff")]
    pub business_day_cutoff: BusinessDayCutoff,
    #[serde(default = "default_reader_pool_size")]
    pub reader_pool_size: NonZeroUsize,
    #[serde(default)]
    pub dev_actor_stub: bool,
}

fn default_reader_pool_size() -> NonZeroUsize {
    NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN)
}

fn timezone<'de, D: Deserializer<'de>>(de: D) -> Result<StoreTimeZone, D::Error> {
    let value = String::deserialize(de)?;
    parse_timezone(&value).map_err(serde::de::Error::custom)
}

fn business_day_cutoff<'de, D: Deserializer<'de>>(de: D) -> Result<BusinessDayCutoff, D::Error> {
    let value = String::deserialize(de)?;
    parse_business_day_cutoff(&value).map_err(serde::de::Error::custom)
}

pub(crate) fn parse_config(text: &str) -> Result<Config, toml::de::Error> {
    let config: Config = toml::from_str(text)?;
    if config.dev_actor_stub && !cfg!(debug_assertions) {
        return Err(<toml::de::Error as serde::de::Error>::custom(
            "dev_actor_stub is unavailable in release builds",
        ));
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
store_id = "01890a5d-ac96-774b-bcce-b302099a8050"
db_path = "boh.db"
listen_addr = "127.0.0.1:8080"
timezone = "Asia/Shanghai"
business_day_cutoff = "04:00"
"#;

    #[test]
    fn accepts_valid_configuration_and_default_pool_size() {
        let config = parse_config(VALID).unwrap();
        assert_eq!(
            config.store_id.to_string(),
            "01890a5d-ac96-774b-bcce-b302099a8050"
        );
        assert_eq!(config.reader_pool_size.get(), 4);
        let config = parse_config(&format!("{VALID}\nreader_pool_size = 2")).unwrap();
        assert_eq!(config.reader_pool_size.get(), 2);
    }

    #[test]
    fn rejects_non_v7_store_id() {
        assert!(parse_config(&VALID.replace("774b", "474b")).is_err());
    }

    #[test]
    fn rejects_missing_timezone() {
        assert!(parse_config(&VALID.replace("timezone = \"Asia/Shanghai\"", "")).is_err());
    }

    #[test]
    fn rejects_invalid_timezone() {
        for timezone in ["Asia/Shangai", "Etc/Unknown"] {
            assert!(parse_config(&VALID.replace("Asia/Shanghai", timezone)).is_err());
        }
    }

    #[test]
    fn rejects_invalid_business_day_cutoff() {
        for cutoff in ["4:00", "24:00", "04:60"] {
            assert!(parse_config(&VALID.replace("04:00", cutoff)).is_err());
        }
    }

    #[test]
    fn rejects_missing_business_day_cutoff() {
        assert!(parse_config(&VALID.replace("business_day_cutoff = \"04:00\"", "")).is_err());
    }

    #[test]
    fn rejects_zero_reader_pool_size() {
        assert!(parse_config(&format!("{VALID}\nreader_pool_size = 0")).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(parse_config(&format!("{VALID}\nunknown = true")).is_err());
    }

    #[test]
    fn dev_actor_stub_defaults_to_disabled_and_is_debug_only() {
        assert!(!parse_config(VALID).unwrap().dev_actor_stub);
        assert_eq!(
            parse_config(&format!("{VALID}\ndev_actor_stub = true")).is_ok(),
            cfg!(debug_assertions)
        );
    }
}
