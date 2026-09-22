//! The weather layers on the wire (S42, docs/protocol.md "Weather layers"):
//! `set_layers` is per client, answered with `obs`; a client that never
//! sends it never sees one. Providers are pointed at a closed port
//! (`OMASTORM_OBS_BASE`), so nothing leaves the machine.

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const REPLY: Duration = Duration::from_secs(20);

fn read(client: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut line = String::new();
    match client.read_line(&mut line) {
        Ok(0) => panic!("engine closed the connection"),
        Ok(_) => Some(serde_json::from_str(&line).unwrap()),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
        Err(e) => panic!("{e}"),
    }
}

/// The next line of `kind` within `wait`, skipping others.
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

#[test]
fn layers_are_per_client_and_answered_with_obs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/t-obs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("cache")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let log_path = root.join("stderr.log");
    let child = Command::new(env!("CARGO_BIN_EXE_omastorm-engine"))
        .env("XDG_RUNTIME_DIR", &root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("OMASTORM_OBS_BASE", "http://127.0.0.1:9")
        .env_remove("OMASTORM_FROST_CLIENT_ID")
        .env_remove("OMASTORM_ARCHIVE")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log_path).unwrap())
        .spawn()
        .unwrap();
    // Killed however the test ends, so a failure leaves no daemon behind.
    let _guard = Guard(child, root.clone());
    let connect = || {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(stream) = UnixStream::connect(root.join("omastorm-se/engine.sock")) {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                return BufReader::new(stream);
            }
            assert!(Instant::now() < deadline, "engine startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
    };
    let mut old = connect();
    let mut new = connect();
    assert!(next_of(&mut old, "hello", REPLY).is_some());
    assert!(next_of(&mut new, "hello", REPLY).is_some());

    send(
        &mut new,
        json!({"type":"set_layers","temp":true,"source":"grid"}),
    );
    let error = next_of(&mut new, "error", REPLY).expect("grid is refused until S43");
    assert_eq!(error["command"], "set_layers");
    send(
        &mut new,
        json!({"type":"set_layers","temp":true,"source":"model"}),
    );
    assert!(next_of(&mut new, "error", REPLY).is_some());

    send(
        &mut new,
        json!({"type":"set_layers","temp":true,"wind":true}),
    );
    let obs = next_of(&mut new, "obs", REPLY).expect("obs after set_layers");
    assert_eq!(obs["v"], 2);
    assert_eq!(obs["source"], "stations");
    assert_eq!(obs["stations"], json!([]));
    let providers = obs["providers"].as_array().unwrap();
    let status = |id: &str| {
        providers.iter().find(|p| p["id"] == id).unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(status("smhi"), "failed");
    assert_eq!(status("fmi"), "failed");
    assert_eq!(status("dmi"), "failed");
    assert_eq!(status("frost"), "skipped");

    // The client that never said set_layers hears nothing new.
    assert!(next_of(&mut old, "obs", Duration::from_secs(2)).is_none());

    // Off and on again: the held list comes back at once.
    send(&mut new, json!({"type":"set_layers"}));
    send(&mut new, json!({"type":"set_layers","wind":true}));
    assert!(next_of(&mut new, "obs", Duration::from_secs(3)).is_some());

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(log.contains("Obs frost: skipped"), "{log}");
    assert!(log.contains("Obs smhi: requests=4 failed"), "{log}");
    assert_eq!(log.matches("Obs fmi: requests=1").count(), 1, "{log}");
}

struct Guard(std::process::Child, PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = fs::remove_dir_all(&self.1);
    }
}
