use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// The Navidrome (OpenSubsonic) server whose logins we trust, e.g. `https://music.example.com`.
    pub navidrome_url: String,
    pub bind: String,
    pub data_dir: PathBuf,
    pub session_ttl_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let navidrome_url = std::env::var("NAVIDROME_URL").map_err(|_| "set NAVIDROME_URL (the Navidrome address)".to_string())?;
        let navidrome_url = navidrome_url.trim_end_matches('/').to_string();
        if !(navidrome_url.starts_with("https://") || navidrome_url.starts_with("http://")) {
            return Err("NAVIDROME_URL must start with http:// or https://".into());
        }
        Ok(Config {
            navidrome_url,
            bind: std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:4540".into()),
            data_dir: std::env::var("DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("data")),
            session_ttl_secs: 12 * 3600,
        })
    }

    /// Is the Navidrome address plain http to a machine that is not this one? Credentials would travel unprotected.
    pub fn navidrome_is_unprotected(&self) -> bool {
        let Some(rest) = self.navidrome_url.strip_prefix("http://") else { return false };
        let host = rest.split(['/', ':']).next().unwrap_or("");
        !(host == "localhost" || host == "127.0.0.1" || host == "[::1]")
    }
}
