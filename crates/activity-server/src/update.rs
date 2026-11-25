//! Knowing when a newer version exists, and checking that a downloaded release really comes from the maintainer.
//!
//! The service never replaces itself: that is left to systemd, Docker and `deploy/update.sh`. All it does is tell the
//! administrator (one log line, or `--check-update`) and verify signatures (`--verify`).

use minisign_verify::{PublicKey, Signature};

/// The key every release file is signed with. A new key would come with a release signed by the old one.
pub const PUBLIC_KEY: &str = "RWSxvwYiiiQcQQMP9+Pl6QpJi5DVfg8ibrI1Ms4Y2eOx6qD1/xQ8ta4/";

pub const REPOSITORY: &str = "ennuiLife/SoundEra-Activity";

/// `1.2.3` or `v1.2.3` as numbers; anything else is not a version we compare.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().strip_prefix('v').unwrap_or(text.trim()).split('.');
    let version = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(version)
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}

/// Does `signature` (the text of a `.minisig` file) belong to `data` under `key`? Returns the signed ("trusted")
/// comment, which names the version and the file, so a genuine old file cannot be passed off as a newer one.
pub fn verify(key: &str, data: &[u8], signature: &str) -> Option<String> {
    let (Ok(key), Ok(signature)) = (PublicKey::from_base64(key), Signature::decode(signature)) else { return None };
    key.verify(data, &signature, false).ok()?;
    Some(signature.trusted_comment().to_string())
}

#[derive(serde::Deserialize)]
struct Latest {
    tag_name: String,
}

/// The newest release's version, from GitHub (one request; GitHub allows 60 an hour per address without a login).
pub async fn latest_version(http: &reqwest::Client) -> Result<String, String> {
    let r = http
        .get(format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("soundera-activity/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !r.status().is_success() {
        return Err(format!("GitHub answered {}", r.status().as_u16()));
    }
    let body = r.bytes().await.map_err(|e| e.without_url().to_string())?;
    if body.len() > 2 * 1024 * 1024 {
        return Err("the answer was too big".into());
    }
    let latest: Latest = serde_json::from_slice(&body).map_err(|_| "the answer was not understood".to_string())?;
    parse_version(&latest.tag_name).map(|_| latest.tag_name).ok_or_else(|| "the newest version has an odd name".to_string())
}

/// One sentence for the log, or `None` when there is nothing to say.
pub fn notice(current: &str, latest: &str) -> Option<String> {
    is_newer(current, latest).then(|| format!("a newer version is available: {latest} (this is {current}). See https://github.com/{REPOSITORY}/releases or run deploy/update.sh"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.12.3"), Some((1, 12, 3)));
        assert!(is_newer("0.1.0", "v0.1.1"));
        assert!(is_newer("0.9.0", "0.10.0"), "numbers, not text");
        assert!(!is_newer("0.2.0", "v0.2.0"));
        assert!(!is_newer("0.3.0", "v0.2.9"));
        for odd in ["", "latest", "1.2", "1.2.3.4", "v1.2.x", "1.2.3-rc1"] {
            assert_eq!(parse_version(odd), None, "{odd:?}");
            assert!(!is_newer("0.1.0", odd));
        }
    }

    #[test]
    fn the_notice_only_speaks_when_there_is_something_newer() {
        assert!(notice("0.1.0", "v0.2.0").unwrap().contains("v0.2.0"));
        assert_eq!(notice("0.2.0", "v0.2.0"), None);
        assert_eq!(notice("0.2.0", "garbage"), None);
    }

    // A throwaway key made for these tests; it signed "release file contents" as version:v1.2.3 file:f.bin.
    const TEST_KEY: &str = "RWQJYvLJHlYU2MgYR8feQEqaMZK6M5xhCikvzY/D0cWYSsF+HUiDcpdd";
    const TEST_SIG: &str = "untrusted comment: signature from rsign secret key\nRUQJYvLJHlYU2FD2gpmvXik3p1pPRmaMmCdvuN1lCX25gvbwqppgJpvHSHsxLncgn3Iz7AsT/SRLVSva0VXhtUfdgqCpSrxfZAc=\ntrusted comment: version:v1.2.3 file:f.bin\nEodked/1pB9Tkh3bdhwTeiocMtJcyTTsQdWVw5wMcgvWXXdwi52VkiUUYIi5wc0iwXtgGL4O4SaNDQSH2eQmBQ==\n";

    #[test]
    fn a_good_signature_gives_its_signed_comment_and_anything_else_is_refused() {
        assert_eq!(verify(TEST_KEY, b"release file contents", TEST_SIG).as_deref(), Some("version:v1.2.3 file:f.bin"));
        assert_eq!(verify(TEST_KEY, b"release file contentz", TEST_SIG), None, "changed file");
        assert_eq!(verify(PUBLIC_KEY, b"release file contents", TEST_SIG), None, "another key");
        assert_eq!(verify(TEST_KEY, b"release file contents", "not a signature"), None);
        assert_eq!(verify("not a key", b"release file contents", TEST_SIG), None);
    }
}
