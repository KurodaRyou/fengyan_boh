//! 假的 NTP 服务（本机 UDP）：按设定的偏差回复，或不回复、回复 KoD、回复不合法的报文、在回复前改动 `ManualClock`。
//! 节点校时的锁定测试（docs/domain.md「时间」门店节点校时）经它核对查询的轮次、顺序和结果。
//! `boh-server` 的进程测试经 `#[path]` 引用本文件，以系统时间回复。
#![allow(dead_code)] // 每个测试文件只用到其中一部分。

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use boh_domain::UnixMillis;
use boh_storage::clock::{Clock, ManualClock};

/// 1900-01-01 到 1970-01-01 的秒数。
const NTP_UNIX_DELTA: i64 = 2_208_988_800;

/// 对一次请求的回复。
#[derive(Debug, Clone)]
pub enum Reply {
    /// 合法回复：接收、发送时间戳都是「服务器时间 + offset_ms」。offset 为正表示节点时钟慢。
    Offset(i64),
    /// 收到请求但不回复。
    Silent,
    /// KoD：stratum 0，参考标识为给定的 4 个 ASCII 字符（如 `DENY`、`RSTR`、`RATE`）。
    KissOfDeath(&'static str),
    /// LI = 3（服务器未同步），其余字段合法：`sntpc` 校验不通过。
    Unsynchronized,
    /// 合法回复，偏差 offset_ms：接收时间戳取收到请求时，发送时间戳 = 接收时间戳 + `processing_ms`（服务器声明的处理耗时，
    /// 可为负）；回复之前把 `ManualClock` 的墙钟改动 `jump_ms`（只用 `set`，不推进单调时间）。模拟查询途中本机时钟跳变：
    /// 节点收到回复时的本机时间与发出时相差 `jump_ms`；`sntpc` 的往返时间是 `jump_ms − processing_ms`。
    JumpThenOffset {
        jump_ms: i64,
        offset_ms: i64,
        processing_ms: i64,
    },
}

#[derive(Clone)]
enum TimeSource {
    Manual(ManualClock),
    System(Clock),
}

impl TimeSource {
    fn now(&self) -> i64 {
        match self {
            TimeSource::Manual(clock) => clock.clock().now().0,
            TimeSource::System(clock) => clock.now().0,
        }
    }
}

struct State {
    queue: VecDeque<Reply>,
    default: Reply,
    requests: u64,
}

/// 运行中的假 NTP 服务；释放时停止。
pub struct FakeNtp {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    arrived: Arc<Condvar>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeNtp {
    /// 以 `ManualClock` 的墙钟作为服务器时间。队列为空时按 `default` 回复。
    pub fn manual(clock: &ManualClock, default: Reply) -> FakeNtp {
        Self::start(TimeSource::Manual(clock.clone()), default)
    }

    /// 以系统时间作为服务器时间（进程测试）。
    pub fn system(default: Reply) -> FakeNtp {
        Self::start(TimeSource::System(Clock::system()), default)
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：本机 UDP 端口无法绑定时测试无法进行。
    fn start(time: TimeSource, default: Reply) -> FakeNtp {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let addr = socket.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            queue: VecDeque::new(),
            default,
            requests: 0,
        }));
        let arrived = Arc::new(Condvar::new());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = state.clone();
            let arrived = arrived.clone();
            let stop = stop.clone();
            std::thread::spawn(move || serve(&socket, &time, &state, &arrived, &stop))
        };
        FakeNtp {
            addr,
            state,
            arrived,
            stop,
            thread: Some(thread),
        }
    }

    /// 配置项 `ntp_servers` 中的写法：`127.0.0.1:<port>`。
    pub fn server(&self) -> String {
        self.addr.to_string()
    }

    /// 之后的请求依次按 `replies` 回复，用完后按默认回复。
    #[allow(clippy::unwrap_used)] // 测试辅助：锁中毒说明服务线程已 panic。
    pub fn push(&self, replies: &[Reply]) {
        self.state
            .lock()
            .unwrap()
            .queue
            .extend(replies.iter().cloned());
    }

    #[allow(clippy::unwrap_used)] // 测试辅助：锁中毒说明服务线程已 panic。
    pub fn set_default(&self, reply: Reply) {
        self.state.lock().unwrap().default = reply;
    }

    /// 已收到的请求数（含不回复的）。
    #[allow(clippy::unwrap_used)] // 测试辅助：锁中毒说明服务线程已 panic。
    pub fn requests(&self) -> u64 {
        self.state.lock().unwrap().requests
    }

    /// 等到收到的请求数不少于 `count`；10 秒内没有则 panic。用于异步测试（每 10ms 检查一次，不阻塞运行时）。
    pub async fn wait_requests(&self, count: u64) {
        for _ in 0..1000 {
            if self.requests() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "fake NTP {} received {} requests, expected at least {count}",
            self.addr,
            self.requests()
        );
    }

    /// 同 `wait_requests`，用于同步测试（进程测试）：在条件变量上等待。
    #[allow(clippy::unwrap_used)] // 测试辅助：锁中毒说明服务线程已 panic。
    pub fn wait_requests_blocking(&self, count: u64, timeout: Duration) {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .arrived
            .wait_timeout_while(state, timeout, |state| state.requests < count)
            .unwrap();
        assert!(
            state.requests >= count,
            "fake NTP {} received {} requests, expected at least {count}",
            self.addr,
            state.requests
        );
    }
}

impl Drop for FakeNtp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 一个已关闭的本机 UDP 端口：发往它的请求得不到回复。
#[allow(clippy::unwrap_used)] // 测试夹具：本机 UDP 端口无法绑定时测试无法进行。
pub fn closed_server() -> String {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().to_string()
}

fn serve(
    socket: &UdpSocket,
    time: &TimeSource,
    state: &Mutex<State>,
    arrived: &Condvar,
    stop: &AtomicBool,
) {
    let mut buf = [0u8; 512];
    while !stop.load(Ordering::SeqCst) {
        let Ok((len, peer)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let received_at = time.now();
        let reply = {
            let Ok(mut state) = state.lock() else {
                return;
            };
            state.requests += 1;
            let default = state.default.clone();
            state.queue.pop_front().unwrap_or(default)
        };
        arrived.notify_all();
        if len < 48 {
            continue;
        }
        let request = &buf[..48];
        let packet = match reply {
            Reply::Silent => continue,
            Reply::Offset(offset) => packet(
                request,
                0,
                2,
                b"LOCL",
                received_at + offset,
                time.now() + offset,
            ),
            Reply::KissOfDeath(code) => {
                let mut id = [b' '; 4];
                id.copy_from_slice(&code.as_bytes()[..4]);
                packet(request, 3, 0, &id, received_at, time.now())
            }
            Reply::Unsynchronized => packet(request, 3, 2, b"LOCL", received_at, time.now()),
            Reply::JumpThenOffset {
                jump_ms,
                offset_ms,
                processing_ms,
            } => {
                if let TimeSource::Manual(clock) = time {
                    clock.set(UnixMillis(received_at + jump_ms));
                }
                packet(
                    request,
                    0,
                    2,
                    b"LOCL",
                    received_at + offset_ms,
                    received_at + offset_ms + processing_ms,
                )
            }
        };
        let _ = socket.send_to(&packet, peer);
    }
}

/// 按 RFC 5905 的服务器回复（mode 4，版本与请求相同），原始时间戳回填请求的发送时间戳。
fn packet(
    request: &[u8],
    li: u8,
    stratum: u8,
    ref_id: &[u8; 4],
    receive_ms: i64,
    transmit_ms: i64,
) -> [u8; 48] {
    let version = (request[0] >> 3) & 0b111;
    let mut out = [0u8; 48];
    out[0] = (li << 6) | (version << 3) | 4;
    out[1] = stratum;
    out[2] = request[2];
    out[3] = 0xEC; // precision 2^-20 s
    out[12..16].copy_from_slice(ref_id);
    out[24..32].copy_from_slice(&request[40..48]);
    out[32..40].copy_from_slice(&ntp_timestamp(receive_ms).to_be_bytes());
    out[40..48].copy_from_slice(&ntp_timestamp(transmit_ms).to_be_bytes());
    out
}

/// Unix 毫秒 → NTP 64 位时间戳。小数部分与 `sntpc` 由本机时间生成时间戳的算法相同（微秒 × 2^32 / 10^6，向下取整），
/// 所以同一毫秒尾数的两个时刻之差是精确的整秒数，偏差恰好 60 000 ms 的回复算出的偏差恰好是 60 秒。
fn ntp_timestamp(unix_ms: i64) -> u64 {
    let seconds = unix_ms.div_euclid(1000) + NTP_UNIX_DELTA;
    let micros = u64::try_from(unix_ms.rem_euclid(1000) * 1000).unwrap_or(0);
    let fraction = micros * (1u64 << 32) / 1_000_000;
    ((u64::try_from(seconds).unwrap_or(0) & 0xFFFF_FFFF) << 32) | fraction
}
