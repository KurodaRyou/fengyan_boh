//! `boh-server` 进程测试的辅助代码：以子进程运行编译好的二进制，经配置文件、状态文件、本机 HTTP、系统 `kill` 命令、
//! 假 NTP 服务和 `boh_storage::testing` 的只读连接观察结果。接口见 docs/interfaces.md「boh-server」各节。
#![allow(dead_code)] // 每个测试文件只用到其中一部分辅助函数。

#[path = "../../../boh-app/tests/spec_support/ntp.rs"]
pub mod ntp;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use boh_storage::StorageError;
use boh_storage::rusqlite::Connection;
use ntp::FakeNtp;

pub const STORE_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
pub const OTHER_STORE_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8059";
/// 开发桩身份：店长。
pub const MANAGER_HEADERS: [(&str, &str); 3] = [
    ("X-Dev-Employee-Id", "01890a5d-ac96-774b-bcce-b302099a8101"),
    ("X-Dev-Device-Id", "01890a5d-ac96-774b-bcce-b302099a8201"),
    ("X-Dev-Role", "MANAGER"),
];

/// 一个节点的临时目录与配置文件。
pub struct Paths {
    pub dir: tempfile::TempDir,
    pub config: PathBuf,
    pub db: PathBuf,
    pub backups: PathBuf,
    pub listen: SocketAddr,
}

impl Paths {
    /// 状态文件 `<db_path>-clock-sync`。
    pub fn clock_sync(&self) -> PathBuf {
        let mut path = self.db.as_os_str().to_owned();
        path.push("-clock-sync");
        PathBuf::from(path)
    }

    /// 状态文件内容；不存在时为 `None`。
    #[allow(clippy::unwrap_used)] // 测试辅助：除不存在以外的读取错误说明夹具有误。
    pub fn clock_sync_file(&self) -> Option<String> {
        match std::fs::read_to_string(self.clock_sync()) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read clock sync file: {error}"),
        }
    }

    /// 写入有效的状态文件：上次校时为系统时间的 `ago_ms` 之前。
    #[allow(clippy::unwrap_used)] // 测试夹具：写不进临时目录时测试无法进行。
    pub fn write_clock_sync(&self, ago_ms: i64) -> i64 {
        let at = boh_storage::clock::Clock::system().now().0 - ago_ms;
        std::fs::write(self.clock_sync(), format!("{at}\n")).unwrap();
        at
    }

    /// `<db_path>-wal` 的大小；不存在时为 `None`。
    pub fn wal_size(&self) -> Option<u64> {
        let mut wal = self.db.as_os_str().to_owned();
        wal.push("-wal");
        std::fs::metadata(PathBuf::from(wal)).ok().map(|m| m.len())
    }
}

/// 写配置文件：`listen_addr` 是一个空闲的本机端口，开发桩开启，`ntp_servers` 为给定的服务器（按顺序）。
pub fn paths(servers: &[&FakeNtp]) -> Paths {
    paths_for_store(STORE_ID, servers)
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录、端口或配置文件准备失败时测试无法开始，直接终止。
pub fn paths_for_store(store_id: &str, servers: &[&FakeNtp]) -> Paths {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("boh.db");
    let backups = dir.path().join("backups");
    let config = dir.path().join("config.toml");
    let listen = free_port();
    write_config(&config, store_id, &db, &backups, listen, servers);
    Paths {
        dir,
        config,
        db,
        backups,
        listen,
    }
}

/// 按同样的格式改写配置文件（如换一个 store_id）。
#[allow(clippy::unwrap_used)] // 测试夹具：写不进临时目录时测试无法进行。
pub fn write_config(
    config: &Path,
    store_id: &str,
    db: &Path,
    backups: &Path,
    listen: SocketAddr,
    servers: &[&FakeNtp],
) {
    let list: Vec<String> = servers
        .iter()
        .map(|server| format!("\"{}\"", server.server()))
        .collect();
    std::fs::write(
        config,
        format!(
            "store_id = \"{store_id}\"\ndb_path = {db:?}\nlisten_addr = \"{listen}\"\n\
             timezone = \"Asia/Shanghai\"\nbusiness_day_cutoff = \"04:00\"\nbackup_dir = {backups:?}\n\
             reader_pool_size = 2\ndev_actor_stub = true\nntp_servers = [{list}]\n",
            db = db.display().to_string(),
            backups = backups.display().to_string(),
            list = list.join(", "),
        ),
    )
    .unwrap();
}

#[allow(clippy::unwrap_used)] // 测试夹具：本机端口无法绑定时测试无法进行。
fn free_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn command(args: &[&std::ffi::OsStr]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_boh-server"));
    command
        .args(args)
        .env("RUST_LOG", "off")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// 运行子命令直到结束，返回退出状态。
#[allow(clippy::unwrap_used)] // 测试夹具：无法启动子进程时测试无法进行，直接终止。
pub fn run(args: &[&std::ffi::OsStr]) -> ExitStatus {
    command(args).status().unwrap()
}

/// 启动服务进程 `boh-server <配置文件>`，不等待就绪。
#[allow(clippy::unwrap_used)] // 同上。
pub fn spawn_server(config: &Path) -> Child {
    command(&[config.as_os_str()]).spawn().unwrap()
}

/// 经系统 `kill` 命令发送信号（如 `TERM`、`INT`）。
#[allow(clippy::unwrap_used)] // 测试辅助：kill 命令不可用时测试无法进行。
pub fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill -{name} failed");
}

/// 等待进程退出，最长 `timeout`（真实时间）；超时时强制结束进程并 panic。
#[allow(clippy::unwrap_used)] // 测试辅助：查询子进程状态失败时测试无法进行。
pub async fn wait_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("boh-server did not exit within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 一次 HTTP/1.1 请求（`Connection: close`），返回状态码和响应体；连不上时为 `None`。
pub fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    match body {
        Some(body) => request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None => request.push_str("\r\n"),
    }
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let status = response.split(' ').nth(1)?.parse().ok()?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Some((status, body))
}

/// 轮询 `GET /health` 直到返回 200；10 秒内不成立，或进程先退出，则 panic。
#[allow(clippy::unwrap_used)] // 测试辅助：查询子进程状态失败时测试无法进行。
pub async fn wait_ready(child: &mut Child, addr: SocketAddr) {
    for _ in 0..500 {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("boh-server exited before serving: {status}");
        }
        if let Some((200, _)) = http(addr, "GET", "/health", &[], None) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = child.kill();
    panic!("boh-server did not become ready");
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
pub fn reader(db: &Path) -> Result<Connection, StorageError> {
    boh_storage::testing::open_reader(db)
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
pub fn writer(db: &Path) -> Result<Connection, StorageError> {
    boh_storage::testing::open_writer(db)
}
