//! Lightning on the wire (S44, docs/protocol.md "Lightning"): `set_layers`
//! `lightning` is per client and answered with `lightning`, naming a packed
//! strikes file; nothing is fetched while no client has it on, and at most
//! once a minute while one does. FMI is played by a local server serving the
//! vendored answer with its times moved to now, so nothing leaves the
//! machine.

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REPLY: Duration = Duration::from_secs(20);

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../data/fixtures/lightning/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// The FMI fixture with its strike times moved so the newest is a minute
/// ago (the engine keeps five hours).
fn fmi_now() -> String {
    let text = String::from_utf8(fixture("fmi_lightning_20260923T0534Z.xml.gz")).unwrap();
    let start = text.find("<gmlcov:positions>").unwrap() + "<gmlcov:positions>".len();
    let end = text.find("</gmlcov:positions>").unwrap();
    let rows: Vec<&str> = text[start..end].lines().collect();
    let newest = rows
        .iter()
        .filter_map(|r| r.split_whitespace().nth(2)?.parse::<i64>().ok())
        .max()
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let by = now - 60 - newest;
    // Squeeze the day into the last 4 hours so all of it is kept.
    let oldest = rows
        .iter()
        .filter_map(|r| r.split_whitespace().nth(2)?.parse::<i64>().ok())
        .min()
        .unwrap();
    let span = (newest - oldest).max(1);
    let moved: Vec<String> = rows
        .iter()
        .map(|r| {
            let p: Vec<&str> = r.split_whitespace().collect();
            if p.len() != 3 {
                return r.to_string();
            }
            let t: i64 = p[2].parse().unwrap();
            let t = newest + by - (newest - t) * 4 * 3600 / span;
            format!("{} {} {t}", p[0], p[1])
        })
        .collect();
    format!("{}{}{}", &text[..start], moved.join("\n"), &text[end..])
}

/// A one-thread HTTP server answering every GET with `body`, counting them.
fn serve(body: String) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let head = String::from_utf8_lossy(&request);
            if head.starts_with("GET /wfs?") && head.contains("lightning::multipointcoverage") {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, count)
}

fn read(client: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut line = String::new();
    match client.read_line(&mut line) {
        Ok(0) => panic!("engine closed the connection"),
        Ok(_) => Some(serde_json::from_str(&line).unwrap()),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
        Err(e) => panic!("{e}"),
    }
}

fn next_of(client: &mut BufReader<UnixStream>, kind: &str, wait: Duration) -> Option<Value> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Some(value) = read(client)
            && value["type"] == kind
        {
            return Some(value);
        }
    }
    None
}

fn send(client: &mut BufReader<UnixStream>, command: Value) {
    writeln!(client.get_mut(), "{command}").unwrap();
}

struct Engine {
    child: std::process::Child,
    root: PathBuf,
}

impl Engine {
    fn start(name: &str, env: &[(&str, String)]) -> Engine {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/t-lightning-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cache")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_omastorm-engine"));
        command
            .env("XDG_RUNTIME_DIR", &root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("OMASTORM_OBS_BASE", "http://127.0.0.1:9")
            .env_remove("OMASTORM_LIGHTNING_REPLAY")
            .env_remove("OMASTORM_LIGHTNING_REPLAY_END")
            .env_remove("OMASTORM_ARCHIVE")
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("stderr.log")).unwrap());
        for (key, value) in env {
            command.env(key, value);
        }
        Engine {
            child: command.spawn().unwrap(),
            root,
        }
    }
    fn connect(&self) -> BufReader<UnixStream> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(stream) = UnixStream::connect(self.root.join("omastorm-se/engine.sock")) {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                return BufReader::new(stream);
            }
            assert!(Instant::now() < deadline, "engine startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default()
    }
    fn file(&self, path: &str) -> Vec<u8> {
        fs::read(self.root.join("omastorm-se").join(path)).unwrap()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count_of(bytes: &[u8]) -> usize {
    assert_eq!(&bytes[..4], b"OSL1");
    let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 16 + 16 * count);
    count
}

#[test]
fn lightning_is_per_client_and_fetched_only_while_on() {
    let (port, requests) = serve(fmi_now());
    let engine = Engine::start(
        "live",
        &[(
            "OMASTORM_LIGHTNING_BASE",
            format!("http://127.0.0.1:{port}"),
        )],
    );
    let mut old = engine.connect();
    let mut new = engine.connect();
    let hello = next_of(&mut old, "hello", REPLY).unwrap();
    assert_eq!(hello["lightning"]["attribution"], "FMI NORDLIS, CC BY 4.0");
    assert_eq!(hello["lightning"]["trailS"], 1800);
    assert_eq!(hello["lightning"]["pollS"], 60);
    assert!(next_of(&mut new, "hello", REPLY).is_some());

    // Off by default: no request at all.
    thread::sleep(Duration::from_secs(2));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    // The weather layers alone do not turn it on.
    send(&mut new, json!({"type":"set_layers","temp":true}));
    thread::sleep(Duration::from_secs(1));
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    send(&mut new, json!({"type":"set_layers","lightning":true}));
    let first = next_of(&mut new, "lightning", REPLY).expect("lightning after set_layers");
    assert_eq!(first["v"], 2);
    assert_eq!(first["status"], "ok");
    assert_eq!(first["replay"], false);
    assert_eq!(first["count"], 190, "194 rows, 4 repeats");
    assert_eq!(first["cloudToGround"], 124);
    assert_eq!(first["attribution"], "FMI NORDLIS, CC BY 4.0");
    let path = first["path"].as_str().unwrap();
    assert!(
        path.starts_with("tex/lightning-") && path.ends_with(".bin"),
        "{path}"
    );
    assert_eq!(count_of(&engine.file(path)), 190);
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    // The client that never asked hears nothing.
    assert!(next_of(&mut old, "lightning", Duration::from_secs(2)).is_none());
    // Off and on again within the minute: the held line, no new request.
    send(&mut new, json!({"type":"set_layers","lightning":false}));
    send(
        &mut new,
        json!({"type":"set_layers","lightning":true,"wind":true}),
    );
    let again = next_of(&mut new, "lightning", Duration::from_secs(3)).unwrap();
    assert_eq!(again["path"], first["path"]);
    thread::sleep(Duration::from_secs(2));
    assert_eq!(requests.load(Ordering::SeqCst), 1, "at most once a minute");
    let log = engine.log();
    assert_eq!(
        log.matches("Lightning fmi: requests=1 bytes=").count(),
        1,
        "{log}"
    );
    assert!(log.contains("strikes=190 new=190 held=190"), "{log}");
}

#[test]
fn a_stored_storm_replays_without_a_request() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../data/fixtures/lightning/smhi_lightning_20260705T11Z.json.gz");
    let engine = Engine::start(
        "replay",
        &[
            ("OMASTORM_LIGHTNING_BASE", "http://127.0.0.1:9".into()),
            (
                "OMASTORM_LIGHTNING_REPLAY",
                fixture.to_string_lossy().into_owned(),
            ),
            (
                "OMASTORM_LIGHTNING_REPLAY_END",
                "2026-09-23T06:00:00Z".into(),
            ),
        ],
    );
    let mut client = engine.connect();
    assert!(next_of(&mut client, "hello", REPLY).is_some());
    send(&mut client, json!({"type":"set_layers","lightning":true}));
    let line = next_of(&mut client, "lightning", REPLY).unwrap();
    assert_eq!(line["replay"], true);
    assert_eq!(line["count"], 7759);
    assert_eq!(line["cloudToGround"], 1473);
    assert_eq!(line["newest"], "2026-09-23T06:00:00Z");
    assert_eq!(line["attribution"], "SMHI, CC BY 4.0");
    let path = line["path"].as_str().unwrap();
    assert_eq!(count_of(&engine.file(path)), 7759);
    let log = engine.log();
    assert!(log.contains("Lightning replay: 7759 strikes"), "{log}");
    assert!(!log.contains("Lightning fmi"), "{log}");
}
