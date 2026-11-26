//! 运行产物目录、单实例锁、断点续跑、崩溃钩子（§7.9）。

use anyhow::{anyhow, bail, Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::OffsetDateTime;

use crate::cli::Step;
use crate::util::{iso8601_local, now_iso};

/// 单实例锁句柄。Drop 时释放（关闭 HANDLE）。
///
/// 实现说明（与 §7.9-1 的偏差，已在移交说明里记录）：
/// 方案书原文写的是 `CreateFileW` + `FILE_SHARE_READ` 独占。这里改用
/// `CreateMutexW("Local\\probe_single_instance")`：同样是内核对象级独占，
/// 但不需要处理共享模式与句柄继承的边角情况，也不会在 out 目录被安全软件
/// 扫描时误判。PID 仍写进 `out/.lock` 供排障。
pub struct LockGuard {
    handle: isize,
}

impl LockGuard {
    /// 只读补救命令不抢单实例锁
    pub fn noop() -> LockGuard {
        LockGuard { handle: 0 }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(
                self.handle as *mut _,
            ));
        }
    }
}

/// 取单实例锁。失败时带上占用者 PID（从 `out/.lock` 读）。
pub fn acquire_lock(lock_path: &Path) -> Result<LockGuard> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\probe_single_instance\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(name.as_ptr()))
            .map_err(|e| anyhow!("创建单实例互斥体失败：{e}"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let owner = fs::read_to_string(lock_path).unwrap_or_default();
            let pid = owner.lines().next().unwrap_or("?").to_string();
            let _ = CloseHandle(h);
            bail!("输出目录已被另一个探针实例占用（PID={pid}）。先关闭那个进程再试。");
        }
        // 供排障：写 PID 与启动时间（非独占写，仅诊断用）
        let _ = fs::write(lock_path, format!("{}\n{}", std::process::id(), now_iso()));
        Ok(LockGuard { handle: h.0 as isize })
    }
}

pub struct State {
    pub seq: u64,
    pub step: Option<Step>,
    pub started: Instant,
}

pub struct Session {
    /// `--out` 目录
    pub root: PathBuf,
    /// 本次 session 目录 `out/sessions/<id>`
    pub dir: PathBuf,
    pub id: String,
    state: Mutex<State>,
    hotkey_step: Arc<Mutex<Option<Step>>>,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    seq_counter: AtomicU64,
    _lock: LockGuard,
    /// 是否续用上一次未完成的 session（§7.9 断点续跑）
    pub resumed: bool,
}

impl Session {
    /// 打开（或续用）输出目录，创建本次 session 子目录，写 `out/latest`。
    pub fn open(root: &Path) -> Result<Arc<Session>> {
        fs::create_dir_all(root).with_context(|| format!("创建输出目录失败：{}", root.display()))?;
        let lock = acquire_lock(&root.join(".lock"))?;

        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions)?;

        // 断点续跑（§7.9）：上一次没写完（没有 finished 标记）就接着写同一个目录，
        // 不新建 session —— 否则「跑一半崩了重启」会得到两个半截目录，报告还得人工合并。
        let mut resumed = false;
        let (id, dir) = match Session::latest_dir(root) {
            Some(prev) if !prev.join("finished").exists() => {
                let id = prev
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "session".to_string());
                resumed = true;
                (id, prev)
            }
            _ => {
                let id = OffsetDateTime::now_local()
                    .unwrap_or_else(|_| OffsetDateTime::now_utc())
                    .format(&time::macros::format_description!(
                        "[year][month][day]-[hour][minute][second]"
                    ))
                    .unwrap_or_else(|_| "session".to_string());
                let dir = sessions.join(&id);
                std::fs::create_dir_all(&dir)?;
                // out/latest 存相对路径，避免符号链接权限问题（§7.9-2）
                let latest = root.join("latest");
                let rel = PathBuf::from("sessions").join(&id);
                std::fs::write(&latest, rel.to_string_lossy().as_bytes())
                    .with_context(|| format!("写 {} 失败", latest.display()))?;
                (id, dir)
            }
        };

        // 续跑时 seq 必须接着已有的最大值，不能从 1 重来
        let max_seq = std::fs::read_to_string(dir.join("timeline.jsonl"))
            .ok()
            .map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v.get("seq").and_then(|x| x.as_u64()))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        let session = Arc::new(Session {
            root: root.to_path_buf(),
            dir: dir.clone(),
            id: id.clone(),
            state: Mutex::new(State {
                seq: 0,
                step: None,
                started: Instant::now(),
            }),
            hotkey_step: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            seq_counter: AtomicU64::new(max_seq),
            _lock: lock,
            resumed,
        });

        Ok(session)
    }

    /// 打开一个**已存在**的 session 目录（不新建、不写 latest）。
    /// 用于 report / annotate / mask 这类只读补救命令。
    pub fn from_existing(root: &Path, dir: &Path) -> Result<Arc<Session>> {
        let id = dir
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".to_string());
        let max_seq = std::fs::read_to_string(dir.join("timeline.jsonl"))
            .ok()
            .map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v.get("seq").and_then(|x| x.as_u64()))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        Ok(Arc::new(Session {
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            id,
            state: Mutex::new(State { seq: max_seq, step: None, started: Instant::now() }),
            hotkey_step: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            seq_counter: AtomicU64::new(max_seq),
            _lock: LockGuard::noop(),
            resumed: false,
        }))
    }

    /// 读 `out/latest` 指向的 session 目录（`report` 命令用）。
    pub fn latest_dir(root: &Path) -> Option<PathBuf> {
        let latest = fs::read_to_string(root.join("latest")).ok()?;
        let p = root.join(latest.trim());
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    }

    /// 所有 session 目录，按名字（时间）排序（§7.9-4）。
    pub fn all_dirs(root: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(root.join("sessions"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn next_seq(&self) -> u64 {
        self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn mono_ms(&self) -> u64 {
        self.state.lock().unwrap().started.elapsed().as_millis() as u64
    }

    pub fn started(&self) -> Instant {
        self.state.lock().unwrap().started
    }

    pub fn step(&self) -> Option<Step> {
        self.state.lock().unwrap().step
    }

    pub fn set_step(&self, s: Step) {
        self.state.lock().unwrap().step = Some(s);
        *self.hotkey_step.lock().unwrap() = Some(s);
    }

    /// 热键线程投递过来的最新 step（消费一次）。
    pub fn take_hotkey_step(&self) -> Option<Step> {
        self.hotkey_step.lock().unwrap().take()
    }

    pub fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_flag.load(Ordering::SeqCst)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// 报告与证据文件的绝对路径（写进 report.md 的附录索引用）。
    pub fn rel_from_root(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub fn ensure_subdir(&self, name: &str) -> Result<PathBuf> {
        let p = self.dir.join(name);
        fs::create_dir_all(&p)?;
        Ok(p)
    }
}

/// 装崩溃钩子（§7.9-5）。必须尽早调用。
pub fn install_panic_hook(out_root: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = Session::latest_dir(&out_root)
            .unwrap_or_else(|| out_root.join("sessions").join("unknown"));
        let _ = fs::create_dir_all(&dir);
        let mut f = match File::create(dir.join("crash.txt")) {
            Ok(f) => f,
            Err(_) => {
                default_hook(info);
                return;
            }
        };
        let _ = writeln!(f, "time: {}", now_iso());
        let _ = writeln!(f, "thread: {:?}", std::thread::current().name());
        let _ = writeln!(f, "panic: {info}");
        let _ = writeln!(f, "location: {:?}", info.location());
        let _ = writeln!(f, "\n--- last events ---");
        for line in crate::recorder::last_lines() {
            let _ = writeln!(f, "{line}");
        }
        let _ = f.flush();
        default_hook(info);
    }));
}

/// `--out` 目录是否可写（preflight 用）
pub fn dir_writable(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(".write_test");
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)?;
    Ok(())
}

/// 供 preflight / report 使用的 session 目录名列表（人读）
pub fn describe_dirs(root: &Path) -> String {
    let dirs = Session::all_dirs(root);
    if dirs.is_empty() {
        return "（无）".to_string();
    }
    dirs.iter()
        .map(|d| d.file_name().unwrap_or_default().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// 收尾完成标记：下次启动据此决定「续跑」还是「新开」（§7.9）
pub fn mark_finished(sess: &Session) {
    let _ = fs::write(sess.path("finished"), now_iso());
}

pub fn _unused_iso() -> String {
    iso8601_local(OffsetDateTime::now_utc())
}
