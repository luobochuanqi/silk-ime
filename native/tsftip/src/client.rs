//! 命名管道客户端：读线程 + 请求通道 + 后台连接线程。
//!
//! TSF 回调线程只做一次有界等待（30 ms），协议读写、连接与 host 拉起全部落在
//! 独立线程上：回调路径永不等待连接建立，未就绪时按键走 fail-open 直通，连接
//! 就绪后由下一次请求自动接上。
//!
//! 句柄以 `FILE_FLAG_OVERLAPPED` 打开，读写全部走重叠 I/O：同步句柄上「一线程
//! 阻塞在 ReadFile、另一线程 WriteFile」会被内核串行化而互相锁死（实测写侧无限
//! 阻塞），重叠 I/O 让读写各自排队，常驻读与并发写不再互斥。
//!
//! 传输是字节模式 + u32 长度前缀自定界（04「传输层」）：一帧一次写，读侧按前缀
//! 重组，不依赖管道的消息边界。
//!
//! 应答关联靠请求 id：每个请求分配递增 id，host 的应答帧回带同一 id，以 update 或
//! error 收尾。id 不符的帧（超时请求的迟到应答）直接丢弃，不依赖任何时序假设。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use silk_protocol::{decode, encode, Envelope};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_BUSY, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Threading::{
    CreateEventW, CreateProcessW, ResetEvent, WaitForSingleObject, CREATE_NO_WINDOW,
    PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::key::Mode;
use crate::msg::{self, Incoming};
use crate::util;

/// 热路径硬超时：超时即放行按键，不丢键（04 降级矩阵）。
///
/// 30 ms 与端到端预算（p95 ≤ 30 ms）对齐：15 ms 会贴着 Windows 调度量子，
/// 单次抢占就会超时放行、字母裸落（真机 14% 脏数据率）；代价是真死等时
/// 按键最多晚 30 ms 直通，肉眼不可感。
pub const KEY_TIMEOUT: Duration = Duration::from_millis(30);
/// 拉起 host 后等 `welcome` 就绪的上限。
const SPAWN_WAIT: Duration = Duration::from_secs(2);
/// 空转时等命令的切片：worker 借此检查后台连接与空闲 ping。
const WORKER_TICK: Duration = Duration::from_millis(500);
/// 空闲存活探测间隔（04:72）：无任何请求时每 30 s 发一次 ping。
const IDLE_PING: Duration = Duration::from_secs(30);
/// host 拉起限速：每分钟最多 3 次。
const SPAWN_BUDGET: usize = 3;
const SPAWN_WINDOW: Duration = Duration::from_secs(60);
/// 单次 ReadFile 的缓冲；字节模式下一次读可能跨多帧或半帧，按前缀重组。
const BUF_SIZE: usize = 64 * 1024;
/// 重叠等待的切片：读线程据此察觉关闭，关闭路径另用 CancelIoEx 立即唤醒。
const WAIT_SLICE_MS: u32 = 20;
/// 单次写（头或体）的上限：发布请求不该被一个卡住的 host 无限拖住。
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// 一次请求的结果。
pub enum Reply {
    /// 本次请求收到的下行消息，按到达顺序。
    Messages(Vec<Incoming>),
    /// 超时未回应：按键应原样放行。
    Timeout,
    /// 管道不可用（未连接或写失败）。
    Disconnected,
}

impl Reply {
    /// 日志用的结果类别。
    pub fn kind(&self) -> &'static str {
        match self {
            Reply::Messages(_) => "messages",
            Reply::Timeout => "timeout",
            Reply::Disconnected => "disconnected",
        }
    }
}

/// 当前焦点会话，供连接（重）建立时补发 `focus_in`。
///
/// 建连在后台线程上异步完成，焦点进入回调多半拿不到连接；没有这一步，新建的
/// 连接不知道当前 sid，之后每个 `key` 都只会收到「未知会话」。
#[derive(Clone)]
pub struct Focus {
    pub sid: String,
    pub app: String,
    pub hwnd: u64,
}

/// 跨线程共享的会话事实。TSF 回调只读这些，避免每键回查连接。
pub struct Shared {
    mode: Mutex<Mode>,
    page_size: AtomicU32,
    connected: AtomicBool,
    /// 累计失败次数，供诊断读取（08）。
    failures: AtomicU32,
    focus: Mutex<Option<Focus>>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            mode: Mutex::new(Mode::default()),
            page_size: AtomicU32::new(5),
            connected: AtomicBool::new(false),
            failures: AtomicU32::new(0),
            focus: Mutex::new(None),
        }
    }

    /// 记录（或清除）当前焦点会话；连接建立后由后台线程补发 `focus_in`。
    pub fn set_focus(&self, focus: Option<Focus>) {
        *self.focus.lock().unwrap_or_else(|e| e.into_inner()) = focus;
    }

    fn focus(&self) -> Option<Focus> {
        self.focus.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn mode(&self) -> Mode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_mode(&self, chinese: bool, full_punct: bool) {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner()) = Mode {
            chinese,
            full_punct,
        };
    }

    pub fn set_mode_if_changed(&self, chinese: bool, full_punct: bool) {
        let current = self.mode();
        if current.chinese != chinese || current.full_punct != full_punct {
            self.set_mode(chinese, full_punct);
        }
    }

    pub fn page_size(&self) -> u32 {
        self.page_size.load(Ordering::Relaxed)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn failures(&self) -> u32 {
        self.failures.load(Ordering::Relaxed)
    }
}

struct Command {
    env: Envelope,
    /// 调用方放弃等待的时刻；`None` 表示只发不收（post）。
    deadline: Option<Instant>,
    reply: Option<Sender<Reply>>,
}

/// 后台连接线程与 IO 线程共享的连接槽。
///
/// 建连（含 `CreateProcess` 拉起 host 与等 `welcome`，上限 2 s）只在这个槽的
/// 后台线程上发生，TSF 回调路径永远不等它：未就绪时按键直接 fail-open。
struct Link {
    /// 已建好、尚未被 IO 线程取走的连接。
    ready: Handoff,
    state: Mutex<LinkState>,
    wake: Condvar,
}

/// 跨线程移交的连接。
///
/// `Pipe` 持有 Win32 句柄（`HANDLE` 是裸指针，windows-rs 不给它 `Send`），但句柄
/// 本身可跨线程使用；这里的所有权由 `Mutex` 串行移交——同一时刻只有一个线程持有
/// 连接，不存在并发访问。
struct Handoff(Mutex<Option<Pipe>>);

// SAFETY: 唯一的访问入口是 `Mutex`，`publish`/`take` 都在持锁期间完成整段移交；
// `Pipe` 内部没有线程亲和状态（读线程用裸值共享句柄，见 `Pipe::connect`）。
unsafe impl Send for Handoff {}
// SAFETY: 同 `Send`：`Mutex` 保证互斥，取出的连接随后只被取走它的线程使用。
unsafe impl Sync for Handoff {}

impl Handoff {
    fn new() -> Handoff {
        Handoff(Mutex::new(None))
    }

    fn take(&self) -> Option<Pipe> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn publish(&self, pipe: Pipe) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(pipe);
    }
}

#[derive(Default)]
struct LinkState {
    /// IO 线程发现需要连接（首个请求或断连后）。
    wanted: bool,
    /// 有连接等着被取走（publish 置位、take 清零），供 IO 线程有界等待。
    has_ready: bool,
    /// 客户端已析构：后台线程退出。
    stopping: bool,
}

impl Link {
    fn new() -> Link {
        Link {
            ready: Handoff::new(),
            state: Mutex::new(LinkState::default()),
            wake: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 请求建连；重复请求合并成一次（不会排队多次拉起）。
    fn request(&self) {
        self.state().wanted = true;
        self.wake.notify_all();
    }

    fn stop(&self) {
        self.state().stopping = true;
        self.wake.notify_all();
    }

    fn publish(&self, pipe: Pipe) {
        self.ready.publish(pipe);
        self.state().has_ready = true;
        self.wake.notify_all();
    }

    /// 等后台线程交出连接，最多 `timeout`。
    ///
    /// 只有 IO 线程调用：TSF 回调从不进来（它只在自己的 30 ms 预算里等回复）。
    fn wait_ready(&self, timeout: Duration) -> Option<Pipe> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state();
        loop {
            if let Some(pipe) = self.ready.take() {
                state.has_ready = false;
                return Some(pipe);
            }
            if state.stopping {
                return None;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            state = self
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// 协议客户端：IO 线程 + 后台连接线程，克隆的是句柄不是连接。
pub struct Client {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    link: Arc<Link>,
    next_id: AtomicU64,
}

impl Client {
    pub fn new() -> Client {
        let (tx, rx) = mpsc::channel::<Command>();
        let shared = Arc::new(Shared::new());
        let link = Arc::new(Link::new());
        let worker_link = Arc::clone(&link);
        let worker_shared = Arc::clone(&shared);
        // 线程句柄不保存：Client 析构时通知连接线程退出、发送端随之析构，IO 线程
        // 读到通道关闭自行退出。不 join —— 析构可能发生在 TSF 回调线程上，而连接
        // 线程此刻可能正在等 2 s 的 host 拉起。
        let _ = std::thread::Builder::new()
            .name("silk-ipc".to_owned())
            .spawn(move || worker_loop(rx, worker_shared, worker_link));
        let connector_shared = Arc::clone(&shared);
        let connector_link = Arc::clone(&link);
        let _ = std::thread::Builder::new()
            .name("silk-ipc-connect".to_owned())
            .spawn(move || connector_loop(connector_link, connector_shared));
        Client {
            tx,
            shared,
            link,
            next_id: AtomicU64::new(0),
        }
    }

    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// 发一条请求并最多等 `timeout`，请求 id 由本函数分配。未连接时立即返回，
    /// 绝不等待建连。
    pub fn request(&self, env: &Envelope, timeout: Duration) -> Reply {
        let mut env = env.clone();
        env.id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        let cmd = Command {
            env,
            deadline: Some(Instant::now() + timeout),
            reply: Some(tx),
        };
        if self.tx.send(cmd).is_err() {
            return Reply::Disconnected;
        }
        match rx.recv_timeout(timeout) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => Reply::Timeout,
            Err(RecvTimeoutError::Disconnected) => Reply::Disconnected,
        }
    }

    /// 预热连接：只置标志并唤醒后台线程，不等待任何 I/O（TSF 回调可安全调用）。
    pub fn warm(&self) {
        self.link.request();
    }

    /// 不等待回应的消息（caret/cancel/ping）；未连接时丢弃。
    pub fn post(&self, env: &Envelope) {
        let _ = self.tx.send(Command {
            env: env.clone(),
            deadline: None,
            reply: None,
        });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // 通知后台连接线程退出；IO 线程在发送端析构后自行退出。
        self.link.stop();
    }
}

fn worker_loop(rx: Receiver<Command>, shared: Arc<Shared>, link: Arc<Link>) {
    let mut pipe: Option<Pipe> = None;
    let mut last_tx = Instant::now();
    loop {
        let cmd = match rx.recv_timeout(WORKER_TICK) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => {
                // 空闲保活（04:72）：顺带发现对端已经退出。
                if pipe.as_ref().is_some_and(Pipe::closed)
                    || keepalive(&mut pipe, &mut last_tx).is_err()
                {
                    drop_pipe(&mut pipe, &shared, &link, "空闲探测失败");
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // 调用方已超时放行的请求不得再送 host：否则 host 会对已直通给应用的字母
        // 继续组字，下一键冒出一串「幽灵」preedit。
        if cmd.deadline.is_some_and(|at| Instant::now() >= at) {
            log::debug!(id = cmd.env.id, kind = cmd.env.type_.as_str(); "丢弃过期请求");
            continue;
        }
        let mut fresh = false;
        if pipe.is_none() {
            link.request();
            // 只在调用方剩余预算内等后台交出连接：IO 线程被建连拖住会让排队请求全部过期。
            let wait = cmd
                .deadline
                .map_or(Duration::ZERO, |at| at.saturating_duration_since(Instant::now()));
            pipe = link.wait_ready(wait);
            fresh = pipe.is_some();
        }
        let Some(mut conn) = pipe.take() else {
            // 未连上：当场 fail-open（04 降级矩阵），后台按限速继续重试。
            if let Some(reply) = &cmd.reply {
                let _ = reply.send(Reply::Disconnected);
            }
            continue;
        };
        // 新连接补发焦点（host 的会话表是 per-连接状态）。id 为 0：其应答无人认领，
        // 在收包时按 id 不符丢弃，不会混进本次请求的应答。
        if fresh {
            if let Some(focus) = shared.focus() {
                if let Ok(frame) = encode(&msg::focus_in(0, &focus.sid, &focus.app, focus.hwnd)) {
                    let _ = conn.write(&frame);
                }
            }
        }
        last_tx = Instant::now();
        let reply = match cmd.deadline {
            Some(deadline) => exchange(&mut conn, &cmd.env, deadline, &shared),
            None => post_only(&mut conn, &cmd.env),
        };
        if matches!(reply, Reply::Disconnected) {
            drop_pipe(&mut pipe, &shared, &link, "读写失败");
        } else {
            pipe = Some(conn);
        }
        if let Some(tx) = cmd.reply {
            let _ = tx.send(reply);
        }
    }
}

/// 连接因读写失败或对端关闭而不可用：丢弃并让后台线程重连。
fn drop_pipe(pipe: &mut Option<Pipe>, shared: &Shared, link: &Link, reason: &str) {
    *pipe = None;
    shared.connected.store(false, Ordering::Relaxed);
    let failures = shared.failures.fetch_add(1, Ordering::Relaxed) + 1;
    log::warn!(reason, failures; "连接断开，后台重连");
    link.request();
}

/// 一次请求-应答：写一帧，收齐 id 匹配的应答。
fn exchange(pipe: &mut Pipe, env: &Envelope, deadline: Instant, shared: &Shared) -> Reply {
    let started = Instant::now();
    let Ok(frame) = encode(env) else {
        log::error!(kind = env.type_.as_str(); "请求编码失败");
        return Reply::Disconnected;
    };
    if pipe.write(&frame).is_err() {
        return Reply::Disconnected;
    }
    let elapsed_us = || started.elapsed().as_micros() as u64;
    match read_response(env.id, deadline, |wait| pipe.recv(wait), shared) {
        Ok(messages) if messages.is_empty() => {
            let failures = shared.failures.fetch_add(1, Ordering::Relaxed) + 1;
            log::warn!(id = env.id, kind = env.type_.as_str(), waited_us = elapsed_us(), failures;
                "host 应答超时，按键放行");
            Reply::Timeout
        }
        Ok(messages) => {
            log::debug!(id = env.id, kind = env.type_.as_str(), rtt_us = elapsed_us(), frames = messages.len();
                "应答");
            Reply::Messages(messages)
        }
        Err(()) => Reply::Disconnected,
    }
}

/// 只发不收（caret/cancel/ping）。
fn post_only(pipe: &mut Pipe, env: &Envelope) -> Reply {
    let Ok(frame) = encode(env) else {
        return Reply::Disconnected;
    };
    if pipe.write(&frame).is_err() {
        return Reply::Disconnected;
    }
    Reply::Messages(Vec::new())
}

/// 收一次应答。
///
/// 只认 id 匹配的帧；host 保证同一应答先 commit 后 update，update、error 或 welcome
/// （hello 的应答）收尾。
/// id 不符的帧是早先超时请求的迟到应答或无主下行（id 0），一律丢弃。超时返回已收到
/// 的部分（可能为空）：已到的 commit 照常上屏，好过丢字。
fn read_response(
    id: u64,
    deadline: Instant,
    mut recv: impl FnMut(Duration) -> Result<Vec<u8>, RecvTimeoutError>,
    shared: &Shared,
) -> Result<Vec<Incoming>, ()> {
    let mut messages = Vec::new();
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Ok(messages);
        }
        let body = match recv(wait) {
            Ok(body) => body,
            Err(RecvTimeoutError::Timeout) => return Ok(messages),
            Err(RecvTimeoutError::Disconnected) => return Err(()),
        };
        // 畸形帧说明流已错位（04：必须断开），交给外层重连。
        let Ok(env) = decode(&body) else {
            log::warn!("下行帧畸形，断开连接");
            return Err(());
        };
        if env.id != id {
            log::debug!(id = env.id, kind = env.type_.as_str(), want = id; "丢弃非本请求的下行帧");
            continue;
        }
        let incoming = msg::parse_incoming(&env);
        observe(&incoming, shared);
        if let Incoming::Error { code, msg } = &incoming {
            log::warn!(id, code = *code; "host 拒绝请求: {msg}");
        }
        let last = matches!(
            incoming,
            Incoming::Update(_) | Incoming::Error { .. } | Incoming::Welcome(_)
        );
        messages.push(incoming);
        if last {
            return Ok(messages);
        }
    }
}

/// 空闲保活：到点发一条 ping（04:72）。写失败返回错误，调用方丢弃连接。
fn keepalive(pipe: &mut Option<Pipe>, last_tx: &mut Instant) -> std::io::Result<()> {
    let Some(active) = pipe.as_mut() else {
        return Ok(());
    };
    if last_tx.elapsed() < IDLE_PING {
        return Ok(());
    }
    *last_tx = Instant::now();
    let frame = encode(&msg::ping(now_ms())).map_err(|_| std::io::Error::other("ping 编码失败"))?;
    active.write(&frame)
}

/// 单调毫秒时间戳，协议 `ts` 字段用。
pub fn now_ms() -> u64 {
    // SAFETY: GetTickCount64 无参数、无指针、无失败路径，只读系统计数器。
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// 后台连接线程：收到建连请求才动手，成功后把连接交给 IO 线程。
fn connector_loop(link: Arc<Link>, shared: Arc<Shared>) {
    let mut spawns: Vec<Instant> = Vec::new();
    let mut retry_at = Instant::now();
    loop {
        {
            let mut state = link.state();
            loop {
                if state.stopping {
                    return;
                }
                if state.wanted && Instant::now() >= retry_at {
                    break;
                }
                let wait = if state.wanted {
                    retry_at.saturating_duration_since(Instant::now())
                } else {
                    // 无请求时久等：唤醒只来自 request/stop。
                    Duration::from_secs(5)
                };
                let (next, _) = link
                    .wake
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
            }
            state.wanted = false;
        }
        match open_link(&shared, &mut spawns) {
            Some(pipe) => link.publish(pipe),
            // 失败后至少隔 1 s 再试：host 未安装时不让后台线程空转。
            None => retry_at = Instant::now() + Duration::from_secs(1),
        }
    }
}

/// 建一条连接：先连已在跑的 host，没有就限速拉起再连。全程只在本线程上。
fn open_link(shared: &Shared, spawns: &mut Vec<Instant>) -> Option<Pipe> {
    let started = Instant::now();
    let Some(sid) = util::current_user_sid() else {
        log::error!("取当前用户 SID 失败，无法定位管道");
        return None;
    };
    let name = format!(r"\\.\pipe\silk-ime-{sid}");
    let mut pipe = match Pipe::connect(&name) {
        Ok(pipe) => pipe,
        Err(_) => {
            // 无人监听：按注册表安装目录拉起 host，限速 3 次/分钟。
            spawns.retain(|t| t.elapsed() < SPAWN_WINDOW);
            if spawns.len() >= SPAWN_BUDGET {
                log::debug!(budget = SPAWN_BUDGET; "host 拉起次数超限，暂停拉起");
                return None;
            }
            if !spawn_host() {
                return None;
            }
            spawns.push(Instant::now());
            let deadline = Instant::now() + SPAWN_WAIT;
            loop {
                if let Ok(pipe) = Pipe::connect(&name) {
                    break pipe;
                }
                if Instant::now() >= deadline {
                    log::warn!(waited_ms = SPAWN_WAIT.as_millis() as u64; "host 已拉起但管道未就绪");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    if !handshake(&mut pipe, shared) {
        log::warn!("握手失败");
        return None;
    }
    log::info!(elapsed_ms = started.elapsed().as_millis() as u64; "已连接 host");
    Some(pipe)
}

/// 握手：发 `hello`，等 `welcome`（版本不匹配的 `error` 直接放弃）。
fn handshake(pipe: &mut Pipe, shared: &Shared) -> bool {
    let Ok(hello) = encode(&msg::hello()) else {
        return false;
    };
    if pipe.write(&hello).is_err() {
        return false;
    }
    let started = Instant::now();
    loop {
        let remaining = SPAWN_WAIT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        let Ok(body) = pipe.recv(remaining) else {
            return false;
        };
        let Ok(env) = decode(&body) else {
            return false;
        };
        match msg::parse_incoming(&env) {
            Incoming::Welcome(cfg) => {
                shared.page_size.store(cfg.page_size.max(1), Ordering::Relaxed);
                shared.set_mode(cfg.chinese, cfg.full_punct);
                shared.connected.store(true, Ordering::Relaxed);
                shared.failures.store(0, Ordering::Relaxed);
                return true;
            }
            // welcome 之前的别的消息只可能是 error：版本不匹配即放弃连接。
            Incoming::Error { code, msg } => {
                log::warn!(code; "host 拒绝握手: {msg}");
                return false;
            }
            _ => {}
        }
    }
}

/// 把下行消息里的全局事实写进共享状态。
fn observe(incoming: &Incoming, shared: &Shared) {
    match incoming {
        Incoming::Welcome(cfg) => {
            shared.page_size.store(cfg.page_size.max(1), Ordering::Relaxed);
            shared.set_mode(cfg.chinese, cfg.full_punct);
        }
        Incoming::Update(update) => {
            if let Some(mode) = update.mode {
                shared.set_mode_if_changed(mode.chinese, mode.full_punct);
            }
        }
        _ => {}
    }
}

/// 按注册表安装目录拉起 host；失败不改状态（调用方本就在降级路径上）。
fn spawn_host() -> bool {
    let Some(dir) = util::read_install_path().or_else(util::module_dir) else {
        log::error!("找不到安装目录（注册表 InstallPath 与模块路径均不可用）");
        return false;
    };
    let exe = dir.join("silkhost.exe");
    if !exe.is_file() {
        log::error!(exe = exe.display().to_string().as_str(); "host 可执行文件不存在");
        return false;
    }
    let dict = dir.join("dict").join("silk.dict");
    let mut cmdline = util::utf16z(&format!(
        "\"{}\" -dict \"{}\"",
        exe.display(),
        dict.display()
    ));
    let startup = STARTUPINFOW {
        cb: core::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: cmdline 是可写 UTF-16 缓冲（CreateProcessW 只原地改写它），生命周期
    // 覆盖调用；startup/info 是栈上独有的可写结构；句柄不继承（false）。
    let ok = unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(windows::core::PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    };
    if let Err(err) = ok {
        log::error!(exe = exe.display().to_string().as_str(); "拉起 host 失败: {err}");
        return false;
    }
    log::info!(pid = info.dwProcessId, exe = exe.display().to_string().as_str(); "已拉起 host");
    // SAFETY: 两个句柄刚由 CreateProcessW 填写、尚未转交别人；本进程不需要它们，
    // 关掉即释放引用（host 进程本身继续运行）。
    unsafe {
        let _ = CloseHandle(info.hProcess);
        let _ = CloseHandle(info.hThread);
    }
    true
}

/// 重叠操作的事件句柄与 OVERLAPPED。
///
/// 内核从提交到完成期间一直引用这两块内存（读还会直接写进调用方的缓冲区），
/// 所以本值必须在操作收尾之后才能释放；本文件里它都是栈局部量或 [`Pipe`] 的
/// 字段，活到调用 [`settle`] 返回之后，且提交后不再移动。
struct Op {
    ov: OVERLAPPED,
    event: HANDLE,
}
impl Op {
    fn new() -> std::io::Result<Op> {
        // 每个操作独立一份事件：读写并发时不共享信号，取消也只针对本操作。
        // SAFETY: CreateEventW 只返回新句柄，参数均为标量；失败走 Err 分支，
        // 不会留下半初始化状态。
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|_| std::io::Error::other("事件创建失败"))?;
        Ok(Op {
            ov: OVERLAPPED {
                hEvent: event,
                ..Default::default()
            },
            event,
        })
    }
}

impl Drop for Op {
    fn drop(&mut self) {
        // SAFETY: 句柄由 Op::new 独占创建，Drop 是唯一释放点；调用方保证已通过
        // settle（或提交即刻失败/完成），内核不再引用 OVERLAPPED。
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

/// windows crate 把 Win32 错误码包在 HRESULT_FROM_WIN32 里，取低 16 位还原。
fn win32_code(err: &windows::core::Error) -> u32 {
    (err.code().0 as u32) & 0xFFFF
}

/// 取已完成操作的结果。
fn result(handle: HANDLE, op: &Op) -> std::io::Result<u32> {
    let mut done = 0u32;
    // SAFETY: handle 是本次操作提交时用的同一句柄，op.ov 提交后未移动、且调用方
    // 只在事件已置位后进来，done 是本地可写量。
    unsafe { GetOverlappedResult(handle, &op.ov, &mut done, false) }
        .map(|()| done)
        .map_err(|_| std::io::Error::other("管道 I/O 失败"))
}

/// 取消未完成的操作并**等内核放手**：OVERLAPPED 与缓冲区在之前释放就是 UAF。
fn abort(handle: HANDLE, op: &Op) {
    // SAFETY: CancelIoEx 只标记取消，不访问 OVERLAPPED 之外的资源；op 由调用方
    // 保证活到本函数返回。
    unsafe {
        let _ = CancelIoEx(handle, Some(&op.ov));
    }
    let mut done = 0u32;
    // 取消请求可能失败（操作恰好已完成），此时这里立即返回。
    // SAFETY: 阻塞等待到内核真正放手，之后调用方才会释放 OVERLAPPED 与缓冲区。
    unsafe {
        let _ = GetOverlappedResult(handle, &op.ov, &mut done, true);
    }
}

/// 等已排队的操作完成。
///
/// 切片等待而非 INFINITE：读线程空闲时也要能察觉关闭标志；关闭路径同时会
/// CancelIoEx 唤醒等待，两者合起来保证读线程在几十毫秒内退出。
fn settle(
    handle: HANDLE,
    op: &Op,
    deadline: Option<Instant>,
    stopping: Option<&AtomicBool>,
) -> std::io::Result<u32> {
    loop {
        // SAFETY: op.event 是 Op::new 创建的有效事件句柄，本函数独占等待它。
        if unsafe { WaitForSingleObject(op.event, WAIT_SLICE_MS) } == WAIT_OBJECT_0 {
            return result(handle, op);
        }
        if stopping.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            abort(handle, op);
            return Err(std::io::Error::other("读线程停止"));
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            abort(handle, op);
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "管道 I/O 超时",
            ));
        }
    }
}

/// 提交一次重叠读并等完成；`buf` 必须活到本函数返回（内核异步期间直接往里写）。
fn overlapped_read(
    handle: HANDLE,
    op: &mut Op,
    buf: &mut [u8],
    stopping: &AtomicBool,
) -> std::io::Result<u32> {
    // SAFETY: op.event 有效且当前无等待者；重置只是清掉上一次的信号。
    unsafe {
        let _ = ResetEvent(op.event);
    }
    // SAFETY: handle 是重叠模式管道句柄；buf 由调用方保证活到本函数返回；OVERLAPPED
    // 与事件归 op 所有、提交后不移动；内核在完成（或 abort 等到的取消）前一直引用两者。
    match unsafe { ReadFile(handle, Some(buf), None, Some(&mut op.ov)) } {
        Ok(()) => result(handle, op),
        Err(err) if win32_code(&err) == ERROR_IO_PENDING.0 => {
            settle(handle, op, None, Some(stopping))
        }
        Err(_) => Err(std::io::Error::other("管道读失败")),
    }
}

/// 提交一次重叠写并等完成；重叠模式下内核只引用到完成，字节数从结果里取。
fn overlapped_write(
    handle: HANDLE,
    op: &mut Op,
    bytes: &[u8],
    timeout: Duration,
) -> std::io::Result<u32> {
    // SAFETY: 同 overlapped_read：事件有效、当前无等待者。
    unsafe {
        let _ = ResetEvent(op.event);
    }
    // SAFETY: handle 是重叠模式管道句柄；bytes 是本次调用独占的只读切片，内核只在
    // 完成前引用它；OVERLAPPED 与事件归 op 所有，settle 返回前不释放。
    match unsafe { WriteFile(handle, Some(bytes), None, Some(&mut op.ov)) } {
        Ok(()) => result(handle, op),
        Err(err) if win32_code(&err) == ERROR_IO_PENDING.0 => {
            settle(handle, op, Some(Instant::now() + timeout), None)
        }
        Err(_) => Err(std::io::Error::other("管道写入失败")),
    }
}

/// 一条命名管道连接：读线程 + 写侧重叠状态。
struct Pipe {
    handle: HANDLE,
    frames: Receiver<Vec<u8>>,
    /// 写句柄的重叠状态；只有 IO 线程用，无需加锁。
    writer: Op,
    reader: Option<JoinHandle<()>>,
    /// 读线程靠它察觉关闭；句柄的重叠读同时会被 CancelIoEx 打断。
    stopping: Arc<AtomicBool>,
}

impl Pipe {
    fn connect(name: &str) -> std::io::Result<Pipe> {
        let namez = util::utf16z(name);
        let path = PCWSTR(namez.as_ptr());
        let deadline = Instant::now() + SPAWN_WAIT;
        let handle = loop {
            // SAFETY: namez 是 NUL 结尾的 UTF-16 且活到本函数返回；安全属性为 None
            // （默认 ACL）；返回的句柄由下面接管并最终在 Drop 里关闭。
            let result = unsafe {
                CreateFileW(
                    path,
                    (windows::Win32::Foundation::GENERIC_READ
                        | windows::Win32::Foundation::GENERIC_WRITE)
                        .0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    // 重叠句柄：同步句柄上「常驻读 + 并发写」会互相锁死。
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match result {
                Ok(handle) => break handle,
                Err(err) => {
                    let busy = win32_code(&err) == ERROR_PIPE_BUSY.0;
                    if busy && Instant::now() < deadline {
                        // SAFETY: path 与上面同一份存活缓冲，超时值只是毫秒数。
                        unsafe {
                            let _ = WaitNamedPipeW(path, 20);
                        }
                        continue;
                    }
                    return Err(std::io::Error::other("管道连接失败"));
                }
            }
        };

        let writer = match Op::new() {
            Ok(op) => op,
            Err(err) => {
                // SAFETY: handle 尚未交给任何结构，本条分支是唯一所有者。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(err);
            }
        };
        let (tx, frames) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stopping);
        // HANDLE 不是 Send，转成裸值跨线程传：所有权仍归本 Pipe，读线程只借用它
        // 调内核；Pipe::drop 先置停止标志并 CancelIoEx、join 读线程之后才关句柄。
        let raw = handle.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("silk-ipc-read".to_owned())
            .spawn(move || {
                // SAFETY: 句柄值由 Pipe::connect 所有权移交而来，读线程与 Pipe 的
                // 生命周期由 join 保证不重叠。
                let handle = HANDLE(raw as *mut core::ffi::c_void);
                read_loop(handle, tx, reader_stop);
            });
        match spawned {
            Ok(reader) => Ok(Pipe {
                handle,
                frames,
                writer,
                reader: Some(reader),
                stopping,
            }),
            Err(_) => {
                // SAFETY: 线程没建成，句柄仍只属于本函数。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                Err(std::io::Error::other("读线程创建失败"))
            }
        }
    }

    /// 写一帧（含长度前缀），整帧一次写。
    ///
    /// 传输是字节模式 + 长度前缀自定界（04「传输层」）：接收方按前缀重组，不依赖
    /// 写边界，所以不再分两段写（Go 侧 `ipc.WriteFrame` 同样是整帧单次写）。
    fn write(&mut self, frame: &[u8]) -> std::io::Result<()> {
        if frame.len() < 4 {
            return Err(std::io::Error::other("帧短于长度前缀"));
        }
        let written = overlapped_write(self.handle, &mut self.writer, frame, WRITE_TIMEOUT)?;
        if written as usize != frame.len() {
            return Err(std::io::Error::other("管道短写"));
        }
        Ok(())
    }

    /// 取一整帧（含 4 字节长度前缀）。区分超时与断连（读线程已退出）。
    fn recv(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.frames.recv_timeout(timeout)
    }

    /// 读线程已退出（对端关闭或流错位）：连接不可再用。
    fn closed(&self) -> bool {
        self.reader.as_ref().map_or(true, |reader| reader.is_finished())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // SAFETY: 句柄在 join 之前一直有效；取消本句柄上所有挂起的重叠操作，
        // 唤醒可能正阻塞在读线程里的 ReadFile。
        unsafe {
            let _ = CancelIoEx(self.handle, None);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        // SAFETY: 读线程已 join，write 侧只有本线程在用；此刻没有其他调用者，
        // 关句柄即释放连接。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 读线程：只做「读一条消息 → 拆帧 → 交出」。
fn read_loop(handle: HANDLE, tx: Sender<Vec<u8>>, stopping: Arc<AtomicBool>) {
    let mut pending: VecDeque<u8> = VecDeque::new();
    let mut buf = vec![0u8; BUF_SIZE];
    let Ok(mut op) = Op::new() else {
        return;
    };
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        // 句柄所有权归 Pipe：Pipe::drop 会先置停止标志、CancelIoEx，再 join 本线程，
        // 因此这里拿到的一定是仍有效的句柄。
        let Ok(read) = overlapped_read(handle, &mut op, &mut buf, &stopping) else {
            return;
        };
        if read == 0 {
            return;
        }
        pending.extend(&buf[..read as usize]);
        loop {
            if pending.len() < 4 {
                break;
            }
            let size = u32::from_le_bytes([pending[0], pending[1], pending[2], pending[3]]) as usize;
            if size > silk_protocol::MAX_FRAME_SIZE || pending.len() < 4 + size {
                // 超限即流错位（04：必须断开）；帧不完整则等下一次读。
                if size > silk_protocol::MAX_FRAME_SIZE {
                    return;
                }
                break;
            }
            // 交出整帧（含长度前缀）：调用方直接喂 decode，无需二次成形。
            let frame: Vec<u8> = pending.drain(..4 + size).collect();
            if tx.send(frame).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_mode_is_shared() {
        let shared = Shared::new();
        assert!(shared.mode().chinese);
        shared.set_mode_if_changed(false, true);
        assert!(!shared.mode().chinese);
        shared.set_mode_if_changed(false, true);
        assert_eq!(shared.page_size(), 5);
        assert!(!shared.is_connected());
        assert_eq!(shared.failures(), 0);
    }

    #[test]
    fn frame_split_reassembles_across_reads() {
        let body = b"{\"v\":1}";
        let mut wire = Vec::new();
        wire.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wire.extend_from_slice(body);
        // 模拟一次读到 5 字节、一次读到剩余：分片必须在缓冲区里重组成帧。
        let mut pending: VecDeque<u8> = wire[..5].iter().copied().collect();
        pending.extend(&wire[5..]);
        let size = u32::from_le_bytes([pending[0], pending[1], pending[2], pending[3]]) as usize;
        pending.drain(..4);
        let frame: Vec<u8> = pending.drain(..size).collect();
        assert_eq!(frame, body);
    }

    #[test]
    fn timeout_is_reported_not_hung() {
        // 没有连接：request 必须立刻返回 Disconnected/Timeout，不能阻塞。
        let client = Client::new();
        let started = Instant::now();
        let reply = client.request(&crate::msg::hello(), Duration::from_millis(15));
        // 本机没有 host 时是 Timeout/Disconnected；有 host 时 30 ms 内连上属正常。
        assert!(!matches!(reply, Reply::Messages(ref m) if m.is_empty()));
        assert!(started.elapsed() < Duration::from_secs(2), "30 ms 预算未守住");
    }

    #[test]
    fn request_never_waits_for_connection_setup() {
        // 03:83：连接建立（CreateProcess + 等 welcome，上限 2 s）在后台线程，回调
        // 路径只等 30 ms 的响应预算，因此冷启动时首次请求也必须立刻返回。
        let client = Client::new();
        let started = Instant::now();
        for _ in 0..3 {
            // 本机可能已有 host 在跑（返回 Messages 也算通过），关键是耗时。
            let _ = client.request(&crate::msg::hello(), KEY_TIMEOUT);
        }
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "冷启动请求不应等待建连：耗时 {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn observe_updates_mode_from_update() {
        let shared = Shared::new();
        observe(
            &Incoming::Update(crate::msg::UpdatePayload {
                mode: Some(crate::msg::ModePayload {
                    chinese: false,
                    full_punct: false,
                }),
                ..Default::default()
            }),
            &shared,
        );
        assert!(!shared.mode().chinese);
        observe(
            &Incoming::Welcome(crate::msg::WelcomeCfg {
                page_size: 9,
                chinese: true,
                full_punct: true,
            }),
            &shared,
        );
        assert_eq!(shared.page_size(), 9);
        assert!(shared.mode().chinese);
    }

    const ID: u64 = 7;

    /// 按给定帧序列喂 read_response；队列耗尽视为超时。
    fn response_from(frames: Vec<Envelope>) -> Result<Vec<Incoming>, ()> {
        let shared = Shared::new();
        let mut queue: VecDeque<Vec<u8>> = frames
            .iter()
            .map(|env| encode(env).expect("测试封套应可编码"))
            .collect();
        read_response(
            ID,
            Instant::now() + KEY_TIMEOUT,
            move |_| queue.pop_front().ok_or(RecvTimeoutError::Timeout),
            &shared,
        )
    }

    fn update_env(id: u64, preedit: &str) -> Envelope {
        Envelope {
            v: silk_protocol::VERSION,
            id,
            type_: silk_protocol::TYPE_UPDATE.to_owned(),
            sid: Some("s-1".to_owned()),
            payload: serde_json::json!({
                "preedit": preedit,
                "caret": preedit.chars().count(),
                "candidates": [],
                "page_index": 0,
                "mode": {"chinese": true, "full_punct": true},
            }),
        }
    }

    fn commit_env(id: u64, text: &str) -> Envelope {
        Envelope {
            v: silk_protocol::VERSION,
            id,
            type_: silk_protocol::TYPE_COMMIT.to_owned(),
            sid: Some("s-1".to_owned()),
            payload: serde_json::json!({ "text": text }),
        }
    }

    #[test]
    fn commit_then_update_is_one_response() {
        let messages = response_from(vec![commit_env(ID, "好"), update_env(ID, "")])
            .expect("应收到响应");
        assert_eq!(messages.len(), 2, "commit 与收尾 update 都必须到达");
        assert_eq!(messages[0], Incoming::Commit("好".to_owned()));
        assert!(matches!(messages[1], Incoming::Update(_)));
    }

    #[test]
    fn stale_frames_are_dropped() {
        // 上一个超时请求的迟到应答（id 6）与补发焦点的应答（id 0）不得混进本次应答。
        let messages = response_from(vec![
            commit_env(ID - 1, "迟到"),
            update_env(0, ""),
            update_env(ID, "h"),
            update_env(ID + 1, "不该被读到"),
        ])
        .expect("应收到响应");
        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0], Incoming::Update(u) if u.preedit == "h"));
    }

    #[test]
    fn timeout_keeps_partial_commit() {
        // 收到 commit 后超时：已到的上屏不能丢。
        let messages = response_from(vec![commit_env(ID, "好")]).expect("超时不算断连");
        assert_eq!(messages, vec![Incoming::Commit("好".to_owned())]);
        assert!(response_from(Vec::new()).expect("空响应不算断连").is_empty());
    }
}

/// 端到端联调检查：需要先手工起 silkhost.exe（不改用默认管道名以外的路径）。
///
/// 用 `SILK_SMOKE=1 cargo test -p silk-tsftip --lib smoke -- --ignored --nocapture`
/// 触发，验证握手、焦点会话与一次真实按键往返。
#[cfg(test)]
mod smoke {
    use super::*;

    #[test]
    #[ignore = "需要本机已有 silkhost.exe 监听默认管道"]
    fn real_host_roundtrip() {
        if std::env::var("SILK_SMOKE").is_err() {
            return;
        }
        crate::logger::init();
        let client = Client::new();
        let shared = client.shared();

        let reply = client.request(&msg::hello(), Duration::from_millis(500));
        match reply {
            Reply::Messages(messages) => {
                assert!(
                    matches!(messages.first(), Some(Incoming::Welcome(_))),
                    "welcome 应先到"
                );
            }
            other => panic!("握手失败，收到 {} 类回复", match other { Reply::Timeout => "timeout", Reply::Disconnected => "disconnected", _ => "unknown" }),
        }
        assert!(shared.is_connected());
        assert!(shared.page_size() >= 1);

        let sid = "s-smoke";
        let reply = client.request(
            &msg::focus_in(0, sid, "smoke.exe", 0x100),
            Duration::from_millis(500),
        );
        assert!(matches!(reply, Reply::Messages(_)), "focus_in 应有回应");

        let reply = client.request(
            &msg::key(0, sid, 8u16, Some('h'), true, 0, 1),
            Duration::from_millis(500),
        );
        match reply {
            Reply::Messages(messages) => {
                let update = messages
                    .iter()
                    .find_map(|m| match m {
                        Incoming::Update(u) => Some(u),
                        _ => None,
                    })
                    .expect("key 应返回 update");
                assert_eq!(update.preedit, "h");
                assert!(!update.candidates.is_empty(), "h 应有候选");
            }
            other => panic!("按键失败，收到 {} 类回复", match other { Reply::Timeout => "timeout", Reply::Disconnected => "disconnected", _ => "unknown" }),
        }

        client.post(&msg::focus_out(0, sid));
    }
}
