//! Authenticated, bounded native runtime transport for private managed snapshots.
//! Lifecycle integration is separate: this channel never launches or stops a process.
use crate::ownership::{OwnershipRecord, ProcessKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_BUDGET: Duration = Duration::from_secs(30);

// Intentionally no Debug or Serialize: the token must not reach diagnostics.
pub struct Channel {
    port: u16,
    token: String,
    owner: OwnershipRecord,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    id: String,
    pid: u32,
    result: Value,
}
#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    id: &'a str,
    token: &'a str,
    request: &'a Value,
}
struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native request exceeds its byte budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Channel {
    /// The caller must obtain the port from a private launch's readiness record.
    /// Loopback alone is not authentication or proof of process ownership.
    pub fn new(port: u16, token: String, owner: OwnershipRecord) -> Result<Self, String> {
        if port == 0
            || owner.kind != ProcessKind::Game
            || token.len() != 64
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid native channel configuration".into());
        }
        Ok(Self { port, token, owner })
    }
    fn live(&self) -> bool {
        crate::ownership::inspect(self.owner.pid).is_some_and(|live| self.owner.describes(&live))
    }
    pub fn request(&self, input: &Value, budget: Duration) -> Result<Value, String> {
        if budget.is_zero() || budget > MAX_BUDGET || !input.is_object() {
            return Err(
                "native requests require an object and a budget from 1 ns to 30 seconds".into(),
            );
        }
        let deadline = Instant::now() + budget;
        if !self.live() {
            return Err("native game ownership is no longer valid".into());
        }
        let id = crate::random::session_id();
        let mut encoded = BoundedBytes(Vec::new());
        serde_json::to_writer(
            &mut encoded,
            &Request {
                version: 1,
                id: &id,
                token: &self.token,
                request: input,
            },
        )
        .map_err(|_| "native request exceeds 1 MiB or could not be encoded".to_string())?;
        let bytes = encoded.0;
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.port));
        let mut stream =
            TcpStream::connect_timeout(&address, remaining(deadline).map_err(io_message)?)
                .map_err(io_message)?;
        stream.set_nodelay(true).map_err(io_message)?;
        write_all(&mut stream, &(bytes.len() as u32).to_le_bytes(), deadline)
            .map_err(io_message)?;
        write_all(&mut stream, &bytes, deadline).map_err(io_message)?;
        let mut header = [0; 4];
        read_all(&mut stream, &mut header, deadline).map_err(io_message)?;
        let length = u32::from_le_bytes(header) as usize;
        if length == 0 || length > MAX_BYTES {
            return Err("native reply length is outside supported bounds".into());
        }
        let mut bytes = vec![0; length];
        read_all(&mut stream, &mut bytes, deadline).map_err(io_message)?;
        let reply: Reply = serde_json::from_slice(&bytes)
            .map_err(|_| "native reply has invalid JSON or schema".to_string())?;
        if reply.version != 1
            || reply.id != id
            || reply.pid != self.owner.pid
            || !reply.result.is_object()
        {
            return Err("native reply identity or version does not match".into());
        }
        if !self.live() {
            return Err("native game exited or changed identity during the request".into());
        }
        Ok(reply.result)
    }
}
fn io_message(error: io::Error) -> String {
    // Never include peer bytes, source content or authentication values.
    format!("native transport failed ({:?})", error.kind())
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native request deadline reached",
        ))
    } else {
        Ok(remaining)
    }
}
fn write_all(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn read_all(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    fn owner() -> OwnershipRecord {
        let live = crate::ownership::inspect(std::process::id()).unwrap();
        OwnershipRecord {
            session: "transport-fixture".into(),
            executable: live.executable,
            pid: live.pid,
            started: live.started.unwrap(),
            project: std::env::temp_dir(),
            kind: ProcessKind::Game,
        }
    }
    fn mock(
        reply: impl FnOnce(Value) -> Vec<u8> + Send + 'static,
    ) -> (Channel, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let channel = Channel::new(
            listener.local_addr().unwrap().port(),
            "a".repeat(64),
            owner(),
        )
        .unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            let mut body = vec![0; u32::from_le_bytes(header) as usize];
            stream.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            let _ = stream.write_all(&reply(request));
        });
        (channel, worker)
    }
    fn frame(value: Value) -> Vec<u8> {
        let body = serde_json::to_vec(&value).unwrap();
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend(body);
        bytes
    }
    #[test]
    fn matching_native_replies_are_accepted_without_exposing_tokens() {
        let (channel, worker) = mock(|request| {
            assert_eq!(request["token"], "a".repeat(64));
            frame(
                json!({"version":1,"id":request["id"],"pid":std::process::id(),"result":{"ok":true}}),
            )
        });
        assert_eq!(
            channel
                .request(&json!({"op":"inspect"}), Duration::from_secs(3))
                .unwrap()["ok"],
            true
        );
        worker.join().unwrap();
    }
    #[test]
    fn stale_process_identity_is_refused_before_connecting() {
        let mut record = owner();
        record.started.push_str("-wrong");
        let channel = Channel::new(1, "a".repeat(64), record).unwrap();
        assert!(channel
            .request(&json!({"op":"inspect"}), Duration::from_secs(1))
            .unwrap_err()
            .contains("ownership"));
    }
    #[test]
    fn bounds_schema_and_reply_identity_fail_closed() {
        for case in 0..5 {
            let (channel, worker) = mock(move |request| {
                let mut value =
                    json!({"version":1,"id":request["id"],"pid":std::process::id(),"result":{}});
                match case {
                    0 => value["version"] = json!(2),
                    1 => value["id"] = json!("wrong"),
                    2 => value["pid"] = json!(std::process::id().wrapping_add(1)),
                    3 => value["result"] = json!("not an object"),
                    _ => value["token"] = json!("must-not-leak"),
                }
                frame(value)
            });
            let error = channel
                .request(&json!({"op":"inspect"}), Duration::from_secs(3))
                .unwrap_err();
            assert!(!error.contains("must-not-leak"));
            worker.join().unwrap();
        }
        let (channel, worker) = mock(|_| (MAX_BYTES as u32 + 1).to_le_bytes().to_vec());
        assert!(channel
            .request(&json!({"op":"inspect"}), Duration::from_secs(3))
            .unwrap_err()
            .contains("length"));
        worker.join().unwrap();
        let channel = Channel::new(1, "a".repeat(64), owner()).unwrap();
        assert!(channel
            .request(
                &json!({"text":"x".repeat(MAX_BYTES)}),
                Duration::from_secs(1)
            )
            .unwrap_err()
            .contains("exceeds"));
        assert!(channel.request(&json!({}), Duration::ZERO).is_err());
        assert!(channel
            .request(&json!({}), Duration::from_secs(31))
            .is_err());
        assert!(Channel::new(1, "not-a-token".into(), owner()).is_err());
    }
    #[test]
    fn a_slow_peer_cannot_extend_the_total_deadline() {
        let (channel, worker) = mock(|_| {
            std::thread::sleep(Duration::from_millis(1500));
            vec![]
        });
        let started = Instant::now();
        assert!(channel
            .request(&json!({"op":"inspect"}), Duration::from_millis(100))
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        worker.join().unwrap();
    }

    #[test]
    #[ignore = "requires the checksum-pinned Godot runtime; run explicitly in the runtime acceptance gate"]
    fn actual_godot_native_channel() {
        let engine =
            std::env::var_os("AURUM_GODOT").expect("native Godot acceptance requires AURUM_GODOT");
        use std::path::PathBuf;
        use std::process::{Child, Command, Stdio};
        struct Fixture {
            root: PathBuf,
            child: Child,
            channel: Channel,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        fn start(engine: &std::ffi::OsStr) -> Fixture {
            let root = std::env::temp_dir().join(format!(
                "aurum-native-channel-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("project.godot"), "config_version=5\n[application]\nrun/main_scene=\"res://main.tscn\"\n[autoload]\nAurumLive=\"*res://runtime.gd\"\n[rendering]\nrenderer/rendering_method=\"gl_compatibility\"\n").unwrap();
            std::fs::write(root.join("main.tscn"), "[gd_scene load_steps=2 format=3]\n[ext_resource type=\"Script\" path=\"res://main.gd\" id=\"1\"]\n[node name=\"Main\" type=\"Node3D\"]\nscript=ExtResource(\"1\")\n").unwrap();
            std::fs::write(root.join("runtime.gd"), include_bytes!("runtime_bridge.gd")).unwrap();
            std::fs::write(root.join("main.gd"), r#"extends Node3D
@export var speed: float = 3.5
@export var caption: String = ""
var inventory = ["cutter", "cell"]
var enemy_ids = ["watcher-2", "guard-7"]
var cooldowns = {"dash": 1.25}
var mission_step = 3
func aurum_capture_state():
	return {"schema": 1, "position": position, "inventory": inventory, "enemy_ids": enemy_ids, "cooldowns": cooldowns, "mission_step": mission_step}
func aurum_restore_state(state):
	if not state is Dictionary or state.get("schema") != 1 or not state.get("position") is Vector3: return false
	position = state.position
	inventory = state.inventory.duplicate()
	enemy_ids = state.enemy_ids.duplicate()
	cooldowns = state.cooldowns.duplicate()
	mission_step = int(state.mission_step)
	return true
"#).unwrap();
            let ready = root.join("ready.json");
            let mut random = crate::random::bytes(32);
            assert_eq!(random.source, crate::random::Source::OperatingSystem);
            let token: String = random
                .bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            random.bytes.fill(0);
            let log = std::fs::File::create(root.join("runtime.log")).unwrap();
            let mut command = Command::new(engine);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .args(["--headless", "--path"])
                .arg(&root)
                .env("AURUM_NATIVE_TOKEN", &token)
                .env("AURUM_NATIVE_READY", &ready)
                .env("AURUM_NATIVE_FREEZE_MS", "1000")
                .env("APPDATA", root.join("userdata"))
                .env("LOCALAPPDATA", root.join("userdata"))
                .env("XDG_DATA_HOME", root.join("userdata"))
                .env("XDG_CONFIG_HOME", root.join("userdata"))
                .env("XDG_CACHE_HOME", root.join("userdata"))
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap();
            let started = Instant::now();
            let metadata = loop {
                if let Ok(bytes) = std::fs::read(&ready) {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        break value;
                    }
                }
                if started.elapsed() > Duration::from_secs(10)
                    || child.try_wait().unwrap().is_some()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "native Godot readiness failed; log: {}",
                        root.join("runtime.log").display()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(metadata["pid"], child.id());
            assert_eq!(metadata["version"], 1);
            assert!(!std::fs::read_to_string(&ready).unwrap().contains(&token));
            let live = crate::ownership::inspect(child.id()).unwrap();
            let owner = OwnershipRecord {
                session: crate::random::session_id(),
                executable: live.executable,
                pid: live.pid,
                started: live.started.unwrap(),
                project: root.clone(),
                kind: ProcessKind::Game,
            };
            let port = u16::try_from(metadata["port"].as_u64().unwrap()).unwrap();
            Fixture {
                root,
                child,
                channel: Channel::new(port, token, owner).unwrap(),
            }
        }
        let first = start(&engine);
        let budget = Duration::from_secs(3);
        let inspect = first
            .channel
            .request(&json!({"op":"inspect"}), budget)
            .unwrap();
        assert_eq!(inspect["ok"], true);
        let caption = "\u{96ea}".repeat(16_000);
        assert_eq!(first.channel.request(&json!({"op":"apply","changes":[{"path":".","property":"caption","value":caption}]}), budget).unwrap()["ok"], true);
        let inspected = first
            .channel
            .request(&json!({"op":"inspect"}), budget)
            .unwrap();
        let echoed = inspected["properties"]
            .as_array()
            .unwrap()
            .iter()
            .find(|property| property["name"] == "caption")
            .unwrap();
        assert_eq!(echoed["value"], caption);
        let change = json!({"op":"apply","changes":[{"path":".","property":"position","value":{"__aurum_type":"Vector3","value":[4,2,-7]}}]});
        assert_eq!(first.channel.request(&change, budget).unwrap()["ok"], true);
        let captured = first
            .channel
            .request(&json!({"op":"checkpoint","freeze":true}), budget)
            .unwrap();
        assert_eq!(captured["ok"], true);
        assert_eq!(captured["mode"], "custom");
        let second = start(&engine);
        let restored = second
            .channel
            .request(
                &json!({"op":"restore","checkpoint":captured["checkpoint"]}),
                budget,
            )
            .unwrap();
        assert_eq!(restored["ok"], true);
        assert_eq!(restored["complete"], true);
        let next = second
            .channel
            .request(&json!({"op":"checkpoint"}), budget)
            .unwrap();
        assert_eq!(
            next["checkpoint"]["custom"],
            captured["checkpoint"]["custom"]
        );
        let mut invalid = captured["checkpoint"].clone();
        invalid["version"] = json!(2);
        assert_eq!(
            second
                .channel
                .request(&json!({"op":"restore","checkpoint":invalid}), budget)
                .unwrap()["ok"],
            false
        );
        assert_eq!(
            second
                .channel
                .request(&json!({"op":"checkpoint"}), budget)
                .unwrap()["checkpoint"]["custom"],
            next["checkpoint"]["custom"]
        );
        let bad_token = Channel::new(
            first.channel.port,
            "0".repeat(64),
            first.channel.owner.clone(),
        )
        .unwrap();
        assert_eq!(bad_token.request(&change, budget).unwrap()["ok"], false);
        assert_eq!(
            first
                .channel
                .request(&json!({"op":"resume"}), budget)
                .unwrap()["paused"],
            false
        );
        assert_eq!(
            first
                .channel
                .request(&json!({"op":"checkpoint","freeze":"yes"}), budget)
                .unwrap()["ok"],
            false
        );
        assert_eq!(
            first
                .channel
                .request(&json!({"op":"checkpoint","freeze":true}), budget)
                .unwrap()["freeze_lease_ms"],
            1000
        );
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(
            first
                .channel
                .request(&json!({"op":"checkpoint"}), budget)
                .unwrap()["checkpoint"]["paused"],
            false
        );
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, first.channel.port));
        let mut oversized = TcpStream::connect_timeout(&address, budget).unwrap();
        oversized.set_read_timeout(Some(budget)).unwrap();
        oversized
            .write_all(&(MAX_BYTES as u32 + 1).to_le_bytes())
            .unwrap();
        assert!(oversized.read_exact(&mut [0]).is_err());
        assert_eq!(
            first
                .channel
                .request(&json!({"op":"inspect"}), budget)
                .unwrap()["ok"],
            true
        );
        assert!(crate::ownership::inspect(first.child.id()).is_some());
        let evidence = json!({"ok":true,"runtime":engine.to_string_lossy(),"first_pid":first.child.id(),"second_pid":second.child.id(),"captured":captured,"restored":restored,"after":next,
            "scope":"native transport and compatible checkpoint fixture, not supervisor replacement or gameplay quality"});
        std::fs::write(
            first.root.join("evidence.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        println!(
            "native channel evidence: {}",
            first.root.join("evidence.json").display()
        );
    }
}
