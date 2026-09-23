//! The MET Nordic grid on the wire (S43, docs/protocol.md "The MET Nordic
//! grid"): `set_layers` `source` `both` gets a stations `obs` and a grid
//! `obs`; the grid's texture is written under `tex/`; the grid is probed and
//! fetched once, then served from memory. THREDDS is played by a local
//! server answering a synthetic subset at the engine's strides, so nothing
//! leaves the machine.

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REPLY: Duration = Duration::from_secs(30);
const NX: usize = 599; // 1796 points every 3
const NY: usize = 774; // 2321 points every 3
const WX: usize = 75; // every 24
const WY: usize = 97;

fn array(out: &mut Vec<u8>, values: impl ExactSizeIterator<Item = f32>) {
    let n = values.len() as u32;
    out.extend(n.to_be_bytes());
    out.extend(n.to_be_bytes());
    for v in values {
        out.extend(v.to_be_bytes());
    }
}

fn time_part(out: &mut Vec<u8>, seconds: f64) {
    out.extend(1u32.to_be_bytes());
    out.extend(1u32.to_be_bytes());
    out.extend(seconds.to_be_bytes());
}

/// A `.dods` answer shaped like THREDDS': x, y, time, then the structures.
fn subset(seconds: f64) -> Vec<u8> {
    let dds = format!(
        "Dataset {{\n    Float32 x[x = {NX}];\n    Float32 y[y = {NY}];\n    Float64 time[time = 1];\n    Structure {{\n        Float32 air_temperature_2m[time = 1][y = {NY}][x = {NX}];\n    }} air_temperature_2m;\n    Structure {{\n        Float32 wind_speed_10m[time = 1][y = {WY}][x = {WX}];\n    }} wind_speed_10m;\n    Structure {{\n        Float32 wind_direction_10m[time = 1][y = {WY}][x = {WX}];\n    }} wind_direction_10m;\n}} metpplatest/met_analysis_1_0km_nordic_latest.nc;\n\nData:\n"
    );
    let mut out = dds.into_bytes();
    array(&mut out, (0..NX).map(|i| -897442.2 + 3000.0 * i as f32));
    array(&mut out, (0..NY).map(|j| -1104322.0 + 3000.0 * j as f32));
    time_part(&mut out, seconds);
    // Warm in the south, cold in the north: 20 °C to −10 °C.
    array(
        &mut out,
        (0..NX * NY).map(|k| 293.15 - 30.0 * (k / NX) as f32 / NY as f32),
    );
    array(&mut out, (0..WX * WY).map(|_| 5.0f32));
    array(&mut out, (0..WX * WY).map(|_| 270.0f32));
    out
}

fn probe(seconds: f64) -> Vec<u8> {
    let mut out =
        b"Dataset {\n    Float64 time[time = 1];\n} metpplatest/met_analysis_1_0km_nordic_latest.nc;\n\nData:\n"
            .to_vec();
    time_part(&mut out, seconds);
    out
}

/// A one-thread HTTP server: `?time` is the probe, `?time,...` the subset,
/// anything else (the station providers) 404. Counts both.
fn serve(seconds: f64) -> (u16, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (probes, subsets) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (p, s) = (probes.clone(), subsets.clone());
    let body = subset(seconds);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head);
            let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
            let answer = if path.contains(".nc.dods?time,") {
                s.fetch_add(1, Ordering::SeqCst);
                Some(body.clone())
            } else if path.ends_with(".nc.dods?time") {
                p.fetch_add(1, Ordering::SeqCst);
                Some(probe(seconds))
            } else {
                None
            };
            let _ = match answer {
                Some(bytes) => stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .as_bytes(),
                    )
                    .and_then(|_| stream.write_all(&bytes)),
                None => stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            };
        }
    });
    (port, probes, subsets)
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

/// The next `obs` from `source` within `wait`, skipping others.
fn next_obs(client: &mut BufReader<UnixStream>, source: &str, wait: Duration) -> Option<Value> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Some(value) = read(client)
            && value["type"] == "obs"
            && value["source"] == source
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
fn the_grid_is_fetched_once_and_drawn_under_tex() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let hour = (now - now % 3600) as f64;
    let (port, probes, subsets) = serve(hour);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/t-grid-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("cache")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let log_path = root.join("stderr.log");
    let child = Command::new(env!("CARGO_BIN_EXE_omastorm-engine"))
        .env("XDG_RUNTIME_DIR", &root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("OMASTORM_OBS_BASE", format!("http://127.0.0.1:{port}"))
        .env_remove("FROST_CLIENT_ID")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("OMASTORM_ARCHIVE")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log_path).unwrap())
        .spawn()
        .unwrap();
    let _guard = Guard(child, root.clone());
    let connect = || {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(stream) = UnixStream::connect(root.join("omastorm-nord/engine.sock")) {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                return BufReader::new(stream);
            }
            assert!(Instant::now() < deadline, "engine startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
    };
    let mut both = connect();
    let mut stations = connect();

    // Review S6: a stations-only client alone costs the grid nothing.
    send(
        &mut stations,
        json!({"type":"set_layers","temp":true,"wind":true}),
    );
    assert!(next_obs(&mut stations, "stations", REPLY).is_some());
    thread::sleep(Duration::from_secs(2));
    assert_eq!(probes.load(Ordering::SeqCst), 0);
    assert_eq!(subsets.load(Ordering::SeqCst), 0);
    send(
        &mut both,
        json!({"type":"set_layers","temp":true,"wind":true,"source":"both"}),
    );
    // Two lines, in either order: the stations' and the grid's.
    let (mut grid, mut listed) = (None, None);
    let deadline = Instant::now() + REPLY;
    while (grid.is_none() || listed.is_none()) && Instant::now() < deadline {
        // S47: a `loading` line comes first; the settled one follows.
        if let Some(value) = read(&mut both)
            && value["type"] == "obs"
            && value["status"] != "loading"
        {
            match value["source"].as_str() {
                Some("grid") => grid = Some(value),
                Some("stations") => listed = Some(value),
                _ => {}
            }
        }
    }
    assert!(listed.is_some(), "no obs source stations");
    let grid = grid.unwrap_or_else(|| {
        panic!(
            "no obs source grid: {}",
            fs::read_to_string(&log_path).unwrap_or_default()
        )
    });
    assert_eq!(grid["provider"]["status"], "ok", "{}", grid["provider"]);
    assert_eq!(
        (grid["status"].as_str(), grid["percent"].as_u64()),
        (Some("ok"), Some(100))
    );
    assert!(
        grid["ageSeconds"]
            .as_i64()
            .is_some_and(|age| (0..3600).contains(&age))
    );
    assert_eq!(grid["attribution"], "MET Norway (CC BY 4.0)");
    let texture = grid["temperature"]["texture"].as_str().unwrap();
    assert!(texture.starts_with("tex/grid-temp-"));
    let png = fs::read(root.join("omastorm-nord").join(texture)).expect("the texture is written");
    assert!(png.starts_with(b"\x89PNG"));
    assert_eq!(grid["temperature"]["width"], 1200);
    let (lo, hi) = (
        grid["temperature"]["minC"].as_f64().unwrap(),
        grid["temperature"]["maxC"].as_f64().unwrap(),
    );
    assert!(lo >= -10.1 && hi <= 20.1 && hi - lo > 25.0, "{lo} {hi}");
    let points = grid["wind"]["points"].as_array().unwrap();
    assert_eq!(points.len(), WX * WY);
    let first: Vec<f64> = points[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(first, [52.3, 1.92, 5.0, 270.0]);
    assert_eq!(grid["wind"]["spacingKm"], 24.0);
    assert_eq!(grid["wind"]["cols"], WX);
    // A stations-only client never hears of the grid.
    assert!(next_obs(&mut stations, "grid", Duration::from_secs(2)).is_none());

    // Off, then wind from the grid alone: answered at once from memory.
    send(&mut both, json!({"type":"set_layers"}));
    send(
        &mut both,
        json!({"type":"set_layers","wind":true,"source":"grid"}),
    );
    let wind = next_obs(&mut both, "grid", Duration::from_secs(3)).expect("held grid");
    assert!(wind.get("temperature").is_none() && wind["wind"]["points"].is_array());
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(subsets.load(Ordering::SeqCst), 1);
    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert_eq!(
        log.matches("Grid metnordic: requests=2 ").count(),
        1,
        "{log}"
    );
}

struct Guard(std::process::Child, PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = fs::remove_dir_all(&self.1);
    }
}
