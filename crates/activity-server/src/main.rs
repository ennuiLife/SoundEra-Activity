use activity_core::SessionKey;
use activity_server::{config::Config, db, router, AppState};
use std::{net::SocketAddr, os::unix::fs::PermissionsExt};

fn load_or_create_key(dir: &std::path::Path) -> std::io::Result<SessionKey> {
    let path = dir.join("session.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(key) = SessionKey::from_bytes(bytes) {
            return Ok(key);
        }
    }
    let key = SessionKey::generate();
    std::fs::write(&path, key.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(key)
}

/// `soundera-activity --health`: ask the running service on `BIND` whether it is well (exit 0) or not (exit 1). Used by
/// the Docker health check, so the image needs no curl.
fn health_check() -> bool {
    use std::io::{Read, Write};
    let bind = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:4540".into());
    // A service bound to all addresses is reached through the local one.
    let address = bind.replace("0.0.0.0", "127.0.0.1").replace("[::]", "[::1]");
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&match address.parse() { Ok(a) => a, Err(_) => return false }, std::time::Duration::from_secs(3)) else { return false };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if stream.write_all(b"GET /activity/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer.starts_with("HTTP/1.1 200")
}

#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version") | Some("-V") => {
            println!("soundera-activity {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--health") => std::process::exit(if health_check() { 0 } else { 1 }),
        Some("--verify") => {
            // soundera-activity --verify FILE SIGNATURE_FILE: prints the signed comment and exits 0 when the file is
            // genuine (signed with the release key); used by deploy/update.sh.
            let (Some(file), Some(sig)) = (std::env::args().nth(2), std::env::args().nth(3)) else {
                eprintln!("usage: soundera-activity --verify FILE SIGNATURE_FILE");
                std::process::exit(2);
            };
            let (Ok(data), Ok(sig)) = (std::fs::read(&file), std::fs::read_to_string(&sig)) else {
                eprintln!("could not read the files");
                std::process::exit(1);
            };
            match activity_server::update::verify(activity_server::update::PUBLIC_KEY, &data, &sig) {
                Some(comment) => {
                    println!("{comment}");
                    return;
                }
                None => {
                    eprintln!("the signature does not match this file");
                    std::process::exit(1);
                }
            }
        }
        Some("--check-update") => {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build().expect("http client");
            match activity_server::update::latest_version(&http).await {
                Ok(latest) => match activity_server::update::notice(env!("CARGO_PKG_VERSION"), &latest) {
                    Some(words) => println!("{words}"),
                    None => println!("soundera-activity {} is the newest version", env!("CARGO_PKG_VERSION")),
                },
                Err(e) => {
                    eprintln!("could not check: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--help") | Some("-h") => {
            println!("soundera-activity {}\n\nA companion service for SoundEra: live presence, follows, reactions, chat.\nConfigured with environment variables: NAVIDROME_URL (required), BIND, DATA_DIR, RUST_LOG, UPDATE_CHECK (off to stop the daily look for a newer version).\n\n  --health   ask the running service whether it is well (exit code 0 or 1)\n  --version  print the version\n  --check-update  say whether a newer version exists\n  --verify FILE SIG  check a downloaded release file against its signature\n\nSee https://github.com/ennuiLife/SoundEra-Activity", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(other) => {
            eprintln!("soundera-activity: unknown option {other} (try --help)");
            std::process::exit(2);
        }
        None => {}
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("soundera-activity: {e}");
            std::process::exit(2);
        }
    };
    if config.navidrome_is_unprotected() {
        tracing::warn!("NAVIDROME_URL is plain http to another machine: logins travel unprotected. Use https.");
    }
    std::fs::create_dir_all(&config.data_dir).expect("data folder");
    let _ = std::fs::set_permissions(&config.data_dir, std::fs::Permissions::from_mode(0o700));
    let key = load_or_create_key(&config.data_dir).expect("session key");
    let conn = db::open(&config.data_dir.join("activity.db")).expect("database");
    let _ = std::fs::set_permissions(config.data_dir.join("activity.db"), std::fs::Permissions::from_mode(0o600));
    let bind = config.bind.clone();
    let app = router(AppState::new(config, key, conn));
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    if std::env::var("UPDATE_CHECK").map(|v| v.eq_ignore_ascii_case("off")).unwrap_or(false) {
        tracing::info!("looking for newer versions is switched off (UPDATE_CHECK=off)");
    } else {
        // Once a day: one request to GitHub, and one log line when there is something newer. Nothing is downloaded.
        tokio::spawn(async {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build().expect("http client");
            loop {
                match activity_server::update::latest_version(&http).await {
                    Ok(latest) => {
                        if let Some(words) = activity_server::update::notice(env!("CARGO_PKG_VERSION"), &latest) {
                            tracing::warn!("{words}");
                        }
                    }
                    Err(e) => tracing::debug!("could not look for a newer version: {e}"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
            }
        });
    }
    tracing::info!("listening on {bind}");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await
        .expect("serve");
}
