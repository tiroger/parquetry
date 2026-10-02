//! One running app per user (Windows and Linux).
//!
//! macOS routes a second launch to the running app itself (`on_open_urls`). Elsewhere
//! every double-clicked file starts a new process, so the first instance listens on
//! a loopback port and later launches hand it their locations and exit. The port
//! and a random token live in `<config dir>/instance`; a stale file (the app quit)
//! just means the connection fails and the new launch becomes the running instance.
//!
//! Set `PARQUETRY_NEW_INSTANCE=1` to always start a separate instance (development).

use std::hash::{BuildHasher as _, Hasher as _};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;

const MAX_MESSAGE: u64 = 1 << 20;

fn info_path() -> PathBuf {
    crate::settings::Settings::directory().join("instance")
}

fn disabled() -> bool {
    std::env::var_os("PARQUETRY_NEW_INSTANCE").is_some_and(|v| v != "0")
}

/// Hand `locations` to a running instance. Returns true if it took them (this
/// process should exit).
pub fn forward(locations: &[String]) -> bool {
    !disabled() && forward_via(&info_path(), locations)
}

fn forward_via(info_path: &std::path::Path, locations: &[String]) -> bool {
    let Ok(info) = std::fs::read_to_string(info_path) else {
        return false;
    };
    let mut lines = info.lines();
    let (Some(port), Some(token)) = (lines.next().and_then(|p| p.trim().parse::<u16>().ok()), lines.next()) else {
        return false;
    };
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) else {
        return false;
    };
    // Generous: the running app may be busy (or the machine loaded), and giving up
    // here means a second copy starts instead.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut message = format!("{}\n", token.trim());
    for location in locations {
        message.push_str(location);
        message.push('\n');
    }
    if stream.write_all(message.as_bytes()).is_err() || stream.shutdown(std::net::Shutdown::Write).is_err() {
        return false;
    }
    // The running instance acknowledges once it accepted the token.
    let mut reply = String::new();
    stream.read_to_string(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Become the running instance: accept locations from later launches and send
/// them to `tx` (an empty list means "open a window").
pub fn listen(tx: UnboundedSender<Vec<String>>) {
    if !disabled() {
        listen_at(info_path(), tx);
    }
}

fn listen_at(path: PathBuf, tx: UnboundedSender<Vec<String>>) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("single-instance listener unavailable: {error}");
            return;
        }
    };
    let Ok(port) = listener.local_addr().map(|a| a.port()) else {
        return;
    };
    let token = random_token();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, format!("{port}\n{token}\n")) {
        log::warn!("couldn't write {}: {error}", path.display());
        return;
    }
    std::thread::Builder::new()
        .name("parquetry-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Some(locations) = receive(stream, &token)
                    && tx.unbounded_send(locations).is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

fn receive(mut stream: TcpStream, token: &str) -> Option<Vec<String>> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut lines = BufReader::new(stream.try_clone().ok()?.take(MAX_MESSAGE)).lines();
    if lines.next()?.ok()?.trim() != token {
        return None;
    }
    let locations: Vec<String> = lines.map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect();
    let _ = stream.write_all(b"ok\n");
    Some(locations)
}

fn random_token() -> String {
    // RandomState is seeded from the OS; two hashes give 128 random bits.
    let part = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt ^ u64::from(std::process::id()));
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        hasher.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;

    use super::*;

    #[test]
    fn later_launch_hands_over_locations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance");
        assert!(!forward_via(&path, &["/nowhere".into()]), "no instance running yet");

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        listen_at(path.clone(), tx);
        assert!(forward_via(&path, &["/data/a.parquet".into(), "s3://b/k".into()]));
        let got = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(got, vec!["/data/a.parquet".to_string(), "s3://b/k".to_string()]);

        assert!(forward_via(&path, &[]));
        assert_eq!(futures::executor::block_on(rx.next()).unwrap(), Vec::<String>::new());

        // A wrong token is refused and nothing is delivered.
        let port = std::fs::read_to_string(&path).unwrap().lines().next().unwrap().to_string();
        std::fs::write(&path, format!("{port}\nnot-the-token\n")).unwrap();
        assert!(!forward_via(&path, &["/evil".into()]));
        assert!(rx.try_recv().is_err(), "nothing delivered");
    }
}
