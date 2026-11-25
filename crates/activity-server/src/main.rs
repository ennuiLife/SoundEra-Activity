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
        Some("--help") | Some("-h") => {
            println!("soundera-activity {}\n\nA companion service for SoundEra: live presence, follows, reactions, chat.\nConfigured with environment variables: NAVIDROME_URL (required), BIND, DATA_DIR, RUST_LOG.\n\n  --health   ask the running service whether it is well (exit code 0 or 1)\n  --version  print the version\n\nSee https://github.com/ennuiLife/SoundEra-Activity", env!("CARGO_PKG_VERSION"));
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
    tracing::info!("listening on {bind}");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await
        .expect("serve");
}
