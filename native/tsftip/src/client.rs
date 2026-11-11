//! 命名管道客户端：读线程 + 请求通道。
//!
//! TSF 回调线程只做一次有界等待（15 ms），协议读写全部落在独立线程上，保证宿主
//! UI 线程不会被管道或 host 拉起阻塞。会话事实（模式、连接态）用共享状态下发，
//! 读侧不加锁。
//!
//! 句柄以 `FILE_FLAG_OVERLAPPED` 打开，读写全部走重叠 I/O：同步句柄上「一线程
//! 阻塞在 ReadFile、另一线程 WriteFile」会被内核串行化而互相锁死（实测写侧无限
//! 阻塞），重叠 I/O 让读写各自排队，常驻读与并发写不再互斥。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
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
pub const KEY_TIMEOUT: Duration = Duration::from_millis(15);
/// 拉起 host 后等 `welcome` 就绪的上限。
const SPAWN_WAIT: Duration = Duration::from_secs(2);
/// 同一响应常带 update 和 commit 两帧；有 update 后只再等这么点时间收尾帧。
const TAIL_WAIT: Duration = Duration::from_millis(2);
/// host 拉起限速：每分钟最多 3 次。
const SPAWN_BUDGET: usize = 3;
const SPAWN_WINDOW: Duration = Duration::from_secs(60);
/// 单次 ReadFile 的缓冲；消息模式下一次读就是一条消息。
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

/// 跨线程共享的会话事实。TSF 回调只读这些，避免每键回查连接。
pub struct Shared {
    mode: Mutex<Mode>,
    page_size: AtomicU32,
    connected: AtomicBool,
    /// 累计失败次数，供诊断读取（08）。
    failures: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Shared {
            mode: Mutex::new(Mode::default()),
            page_size: AtomicU32::new(5),
            connected: AtomicBool::new(false),
            failures: AtomicU32::new(0),
        }
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
    want_reply: bool,
    reply: Sender<Reply>,
}

/// 协议客户端。内部只有一个 IO 线程，克隆的是句柄不是连接。
pub struct Client {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Client {
    pub fn new() -> Client {
        let (tx, rx) = mpsc::channel::<Command>();
        let shared = Arc::new(Shared::new());
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("silk-ipc".to_owned())
            .spawn(move || worker_loop(rx, worker_shared))
            .ok();
        Client {
            tx,
            shared,
            worker,
        }
    }

    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// 发一条消息并最多等 `timeout`。
    pub fn request(&self, env: &Envelope, want_reply: bool, timeout: Duration) -> Reply {
        let (tx, rx) = mpsc::channel();
        if self
            .tx
            .send(Command {
                env: env.clone(),
                want_reply,
                reply: tx,
            })
            .is_err()
        {
            return Reply::Disconnected;
        }
        match rx.recv_timeout(timeout) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => Reply::Timeout,
            Err(RecvTimeoutError::Disconnected) => Reply::Disconnected,
        }
    }

    /// 不等待回应的消息（caret/ping/bye）。
    pub fn post(&self, env: &Envelope) {
        let (tx, _rx) = mpsc::channel();
        let _ = self.tx.send(Command {
            env: env.clone(),
            want_reply: false,
            reply: tx,
        });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // 丢掉发送端让 IO 线程退出；连接与读线程随之释放。不 join：
        // 宿主进程退出时阻塞在管道读上的线程由连接关闭收尾。
        let _ = &self.worker;
    }
}

fn worker_loop(rx: Receiver<Command>, shared: Arc<Shared>) {
    let mut conn = Connection::default();
    for cmd in rx {
        if cmd.want_reply {
            let reply = conn.handle(&cmd.env, &shared);
            let _ = cmd.reply.send(reply);
        } else {
            let _ = conn.send_only(&cmd.env, &shared);
        }
    }
}

#[derive(Default)]
struct Connection {
    pipe: Option<Pipe>,
    spawns: Vec<Instant>,
}

impl Connection {
    fn send_only(&mut self, env: &Envelope, shared: &Shared) -> Reply {
        if self.pipe.is_none() && !self.connect(shared) {
            return Reply::Disconnected;
        }
        let Some(pipe) = self.pipe.as_mut() else {
            return Reply::Disconnected;
        };
        let Ok(frame) = encode(env) else {
            return Reply::Disconnected;
        };
        if pipe.write(&frame).is_err() {
            self.drop_pipe(shared);
            return Reply::Disconnected;
        }
        Reply::Messages(Vec::new())
    }

    fn handle(&mut self, env: &Envelope, shared: &Shared) -> Reply {
        let started = Instant::now();
        if self.pipe.is_none() && !self.connect(shared) {
            return Reply::Disconnected;
        }
        let mut messages = Vec::new();
        {
            let Some(pipe) = self.pipe.as_mut() else {
                return Reply::Disconnected;
            };
            // 上一键超时留下的迟到响应属于已经放行的按键，这里整批丢弃，
            // 不能混进本次响应（04：迟到响应一律丢弃）。
            pipe.drain();
            let Ok(frame) = encode(env) else {
                return Reply::Disconnected;
            };
            if pipe.write(&frame).is_err() {
                self.drop_pipe(shared);
                return Reply::Disconnected;
            }
            loop {
                let budget = KEY_TIMEOUT.saturating_sub(started.elapsed());
                // 有 preedit 的 update 之后不会再有 commit；其余情况要收尾帧。
                let wait = if messages.is_empty() {
                    budget
                } else {
                    budget.min(TAIL_WAIT)
                };
                if wait.is_zero() {
                    break;
                }
                let Ok(body) = pipe.recv(wait) else {
                    break;
                };
                // 畸形帧说明流已错位（04：必须断开），交给外层重连。
                let Ok(env) = decode(&body) else {
                    self.drop_pipe(shared);
                    return Reply::Disconnected;
                };
                let incoming = msg::parse_incoming(&env);
                observe(&incoming, shared);
                let last = match &incoming {
                    Incoming::Update(update) => !update.preedit.is_empty(),
                    _ => true,
                };
                messages.push(incoming);
                if last {
                    break;
                }
            }
        }
        if messages.is_empty() {
            // 一条都没收到：host 超时，按降级处理。
            shared.failures.fetch_add(1, Ordering::Relaxed);
            Reply::Timeout
        } else {
            Reply::Messages(messages)
        }
    }

    fn connect(&mut self, shared: &Shared) -> bool {
        let Some(sid) = util::current_user_sid() else {
            return false;
        };
        let name = format!(r"\\.\pipe\silk-ime-{sid}");
        if let Ok(pipe) = Pipe::connect(&name) {
            return self.handshake(pipe, shared);
        }
        // 无人监听：按注册表安装目录拉起 host，限速 3 次/分钟。
        self.spawns.retain(|t| t.elapsed() < SPAWN_WINDOW);
        if self.spawns.len() >= SPAWN_BUDGET {
            return false;
        }
        self.spawns.push(Instant::now());
        if !spawn_host() {
            return false;
        }
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            if let Ok(pipe) = Pipe::connect(&name) {
                return self.handshake(pipe, shared);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn handshake(&mut self, mut pipe: Pipe, shared: &Shared) -> bool {
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
            let incoming = msg::parse_incoming(&env);
            if let Incoming::Welcome(cfg) = &incoming {
                shared.page_size.store(cfg.page_size.max(1), Ordering::Relaxed);
                shared.set_mode(cfg.chinese, cfg.full_punct);
                self.pipe = Some(pipe);
                shared.connected.store(true, Ordering::Relaxed);
                shared.failures.store(0, Ordering::Relaxed);
                return true;
            }
            // welcome 之前的别的消息只可能是 error：版本不匹配即放弃连接。
            if matches!(incoming, Incoming::Error { .. }) {
                return false;
            }
        }
    }

    fn drop_pipe(&mut self, shared: &Shared) {
        self.pipe = None;
        shared.connected.store(false, Ordering::Relaxed);
        shared.failures.fetch_add(1, Ordering::Relaxed);
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
        return false;
    };
    let exe = dir.join("silkhost.exe");
    if !exe.is_file() {
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
    if ok.is_err() {
        return false;
    }
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
        // 安全前提：调用方已通过 settle、或提交即失败/已完成，保证内核不再引用。
        unsafe { let _ = CloseHandle(self.event); }
    }
}

/// windows crate 把 Win32 错误码包在 HRESULT_FROM_WIN32 里，取低 16 位还原。
fn win32_code(err: &windows::core::Error) -> u32 {
    (err.code().0 as u32) & 0xFFFF
}

/// 取已完成操作的结果。
fn result(handle: HANDLE, op: &Op) -> std::io::Result<u32> {
    let mut done = 0u32;
    unsafe { GetOverlappedResult(handle, &op.ov, &mut done, false) }
        .map(|()| done)
        .map_err(|_| std::io::Error::other("管道 I/O 失败"))
}

/// 取消未完成的操作并**等内核放手**：OVERLAPPED 与缓冲区在之前释放就是 UAF。
fn abort(handle: HANDLE, op: &Op) {
    unsafe { let _ = CancelIoEx(handle, Some(&op.ov)); }
    let mut done = 0u32;
    // 取消请求可能失败（操作恰好已完成），此时这里立即返回。
    unsafe { let _ = GetOverlappedResult(handle, &op.ov, &mut done, true); }
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
    unsafe { let _ = ResetEvent(op.event); }
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
    unsafe { let _ = ResetEvent(op.event); }
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
                unsafe { let _ = CloseHandle(handle); }
                return Err(err);
            }
        };
        let (tx, frames) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stopping);
        // HANDLE 不是 Send；所有权仍归本 Pipe（Drop 先取消再关句柄、并等读线程
        // 退出），读线程只借用这个值调用内核。
        let raw = handle.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("silk-ipc-read".to_owned())
            .spawn(move || {
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
                unsafe { let _ = CloseHandle(handle); }
                Err(std::io::Error::other("读线程创建失败"))
            }
        }
    }

    /// 写一帧（含长度前缀）。
    ///
    /// 头与体必须分两条消息写：消息模式管道在接收方缓冲不足时会丢弃消息余部，
    /// 而 host 的 ReadFrame 先按 4 字节读头，所以帧体必须是下一条独立消息
    /// （与 Go 侧 WriteFrame 的两段写一一对应）。
    fn write(&mut self, frame: &[u8]) -> std::io::Result<()> {
        if frame.len() < 4 {
            return Err(std::io::Error::other("帧短于长度前缀"));
        }
        let (header, body) = frame.split_at(4);
        self.write_message(header)?;
        self.write_message(body)
    }

    fn write_message(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let written = overlapped_write(self.handle, &mut self.writer, bytes, WRITE_TIMEOUT)?;
        if written as usize != bytes.len() {
            return Err(std::io::Error::other("管道短写"));
        }
        Ok(())
    }

    /// 取一整帧（含 4 字节长度前缀）。超时或断连返回 Err。
    fn recv(&self, timeout: Duration) -> Result<Vec<u8>, ()> {
        self.frames.recv_timeout(timeout).map_err(|_| ())
    }

    /// 丢弃已经到达的所有帧。
    fn drain(&self) {
        while self.frames.try_recv().is_ok() {}
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // 读线程可能正在等事件：取消句柄上所有挂起的重叠操作把它唤醒。
        // 读线程自己也会在切片等待里看到停止标志，因此带宽上界只是切片时长。
        unsafe { let _ = CancelIoEx(self.handle, None); }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        // 读线程已退出，句柄不再被任何人调用；关句柄即释放连接。
        unsafe { let _ = CloseHandle(self.handle); }
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
        let reply = client.request(&crate::msg::hello(), true, Duration::from_millis(15));
        assert!(matches!(reply, Reply::Timeout | Reply::Disconnected));
        // 本机若有 host 在跑，15 ms 预算也必须守住。
        assert!(started.elapsed() < Duration::from_secs(2));
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
        let client = Client::new();
        let shared = client.shared();

        let reply = client.request(&msg::hello(), true, Duration::from_millis(500));
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
            true,
            Duration::from_millis(500),
        );
        assert!(matches!(reply, Reply::Messages(_)), "focus_in 应有回应");

        let reply = client.request(
            &msg::key(0, sid, 8u16, Some('h'), true, 0, 1),
            true,
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
