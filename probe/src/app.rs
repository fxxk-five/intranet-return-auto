//! attach 主循环：把 CDP 事件流分发到各模块，并在结束时产出全部产物。
//!
//! 设计取舍：**单任务事件循环 + 250ms tick**。
//! 不用多线程是因为：① 所有处置都是"记录"，没有重活；② 时序正确（点击与请求
//! 的 500ms 关联窗口）比吞吐重要得多；③ 出问题时栈是单线的，好排查。

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cdp::{commands, Cdp};
use crate::cli::Step;
use crate::correlate::Correlator;
use crate::dialogs::DialogStore;
use crate::annotate::Annotator;
use crate::domino;
use crate::filmstrip::{FilmAfter, Filmstrip};
use crate::inputs::{self, Inputs};
use crate::mirror::Mirror;
use crate::experiment;
use crate::hotkeys;
use crate::netcap::NetStore;
use crate::recorder::Recorder;
use crate::report;
use crate::session::Session;
use crate::shot;
use crate::util::truncate_bytes;
use crate::win32;

pub struct AttachCfg {
    pub out: PathBuf,
    pub port: u16,
    pub include_cookies: bool,
    pub no_screenshot: bool,
    pub step: Option<Step>,
    pub hotkeys: Option<String>,
    // ---- 影印机相关（PROMPT §5、§3、§4、§6）----
    pub numbers: Option<PathBuf>,
    pub values: Option<PathBuf>,
    pub annotate: bool,
    pub plan: Option<PathBuf>,
    pub no_filmstrip: bool,
    pub full_page_shots: bool,
    pub no_debugger: bool,
    pub mirror_max_mb: u64,
    pub filmstrip_max_mb: u64,
    /// 引导模式（收尾轮新增：一趟性任务的保险）
    pub guide: bool,
    /// 只扒源码：零按键模式，操作员只要登录 + 打开页面 + 关浏览器
    pub mirror_only: bool,
    /// 自动巡页：登录后自己把 4 个业务页面打开
    pub auto_pages: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // target_id/title 暂时只用于排障，保留结构完整
struct PageRef {
    target_id: String,
    kind: String,
    url: String,
    title: String,
}

pub struct App {
    sess: Arc<Session>,
    rec: Recorder,
    cdp: Arc<Cdp>,
    cfg: AttachCfg,
    net: NetStore,
    corr: Correlator,
    dialogs: DialogStore,
    pages: HashMap<String, PageRef>,
    jobs_seen: HashSet<u32>,
    tick: u64,
    step_for_snapshot: Option<Step>,
    last_dom_snapshot: Option<String>,
    spool_wait: u32,
    // 影印机
    mirror: Mirror,
    film: Filmstrip,
    inputs: Inputs,
    annotator: Option<Annotator>,
    annot_q: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    film_tx: tokio::sync::mpsc::UnboundedSender<FilmAfter>,
    last_progress: std::time::Instant,
    /// 待抓的运行时脚本（Debugger.scriptParsed 只入队，真正取源码放到 tick 里做，
    /// 否则几十个脚本会把事件循环堵住，操作员的弹窗都来不及处理）
    pending_scripts: std::collections::VecDeque<(String, String, String, Option<String>)>,
    /// 已经镜像过的页面 URL（避免每次导航重复走资源树）
    harvester_done: HashSet<String>,
    /// 录屏环形缓冲（"前"图从这里取，修复 4b）
    screencast: crate::filmstrip::Screencast,
    /// 最近一次 pointerdown 的上下文（等它对应的 click 来认领）
    last_pointer: Option<PointerCtx>,
    /// 引导模式：期望动作序列 + 当前进度
    guide_flow: Option<Vec<(u32, String, String)>>,
    guide_idx: usize,
    /// 会话中提前采到的元素清单（浏览器被关掉后 finish 也能用）
    pre_elements: Vec<(String, Value)>,
    /// 已经预采集过元素清单的页面 URL
    pre_collected: HashSet<String>,
    /// 页面"加载完成后"完整抓过的 URL（可重复抓，靠 mirror 内部去重）
    complete_harvested: HashSet<String>,
    /// 已经存过 MHTML 的 URL（避免重复存档）
    mhtml_done: HashSet<String>,
    // ---- 自动巡页 ----
    auto_done: bool,
    auto_scan_ticks: u32,
    auto_wait_ticks: u32,
    /// 已找到的业务页面名
    auto_found: HashSet<String>,
    /// 已打开但还没抓完的 URL
    auto_unharvested: HashSet<String>,
    /// 上一次扫描时所在的页面 URL：登录跳转后要重新计时
    auto_last_url: String,
    /// 已经抓过完整 DOM（含 iframe）的 URL
    dom_done: HashSet<String>,
    /// 子 iframe 导航后的延迟补抓队列（避免在 0ms 抢跑抓到空白壳或占坑）
    pending_iframe_harvest: std::collections::VecDeque<IframeHarvestJob>,
    /// 同源主动 fetch 兜底已尝试过的 URL（避免对 404 资源无限重发）
    fallback_fetch_tries: HashMap<String, u8>,
    /// 增量落盘 INDEX.json 的节流记录
    last_index_flush: std::time::Instant,
    last_index_fetched: usize,
    /// 现场抓齐实时看板（重点盯防 A 阶段 HTML/JS/提交请求）
    board: CoverageBoard,
}

/// 子 iframe 加载完成后的延迟补抓任务
#[derive(Debug, Clone)]
struct IframeHarvestJob {
    sid: String,
    url: String,
    next_at: std::time::Instant,
    retries: u8,
}

/// 现场核心产物抓齐看板：抓到关键项立即在黑窗口亮灯，防止白跑一趟
#[derive(Debug, Clone, Default)]
struct CoverageBoard {
    a_html: bool,
    a_main_js: bool,
    a_format_js: bool,
    a_submit_post: usize,
    b_submit_post: usize,
    c_submit_post: usize,
    d_submit_post: usize,
    printed_once: bool,
}

impl CoverageBoard {
    /// 判断是否为 A 阶段业务提交请求（排除打开页面自动发的初始化查询）
    fn is_a_submit(url: &str, method: &str, numbers_in_body: usize) -> bool {
        let u = url.to_ascii_lowercase();
        if !u.contains("/setroadseg/") {
            return false;
        }
        if numbers_in_body > 0 {
            return true;
        }
        if !method.eq_ignore_ascii_case("POST") {
            return false;
        }
        // 页面加载自动拉取的初始化接口不算“业务提交”
        !u.contains("getunfinishlist")
            && !u.contains("getselftakeorg")
            && !u.contains("getcarline")
            && !u.contains("getwarehouseinfo")
    }

    fn is_b_submit(url: &str, method: &str, numbers_in_body: usize) -> bool {
        let u = url.to_ascii_lowercase();
        if !u.contains("/waybillprint/") && !u.contains("/dealreceiptbatch/") {
            return false;
        }
        numbers_in_body > 0
            || (method.eq_ignore_ascii_case("POST")
                && (u.contains("query")
                    || u.contains("updatemobile")
                    || u.contains("print")
                    || u.contains("model")))
    }

    fn is_c_submit(url: &str, method: &str, numbers_in_body: usize) -> bool {
        let u = url.to_ascii_lowercase();
        if !u.contains("/signwaybillreturned/") {
            return false;
        }
        numbers_in_body > 0
            || (method.eq_ignore_ascii_case("POST")
                && (u.contains("query") || u.contains("completeconfirm")))
    }

    fn is_d_submit(url: &str, method: &str, numbers_in_body: usize) -> bool {
        let u = url.to_ascii_lowercase();
        if !u.contains("/returnwork/") {
            return false;
        }
        if numbers_in_body > 0 {
            return true;
        }
        method.eq_ignore_ascii_case("POST")
            && (u.contains("querydelivertask")
                || u.contains("acceptdelivertask")
                || u.contains("gainreceiptwaybillno")
                || u.contains("updatereceiptwaybillnobyno"))
    }

    /// 登记一次网络请求，返回“是否有关键里程碑状态发生变化”
    fn observe_request(&mut self, url: &str, method: &str, numbers_in_body: usize) -> bool {
        let mut changed = false;
        if Self::is_a_submit(url, method, numbers_in_body) {
            let prev = self.a_submit_post;
            self.a_submit_post += 1;
            if prev == 0 {
                changed = true;
            }
        }
        if Self::is_b_submit(url, method, numbers_in_body) {
            let prev = self.b_submit_post;
            self.b_submit_post += 1;
            if prev == 0 {
                changed = true;
            }
        }
        if Self::is_c_submit(url, method, numbers_in_body) {
            let prev = self.c_submit_post;
            self.c_submit_post += 1;
            if prev == 0 {
                changed = true;
            }
        }
        if Self::is_d_submit(url, method, numbers_in_body) {
            let prev = self.d_submit_post;
            self.d_submit_post += 1;
            if prev == 0 {
                changed = true;
            }
        }
        changed
    }

    fn format_panel(&self) -> String {
        let ok = |b: bool| if b { "OK" } else { "待抓" };
        let cnt = |n: usize| {
            if n > 0 {
                format!("OK({n}条)")
            } else {
                "待做1单".to_string()
            }
        };
        let a_ready = self.a_html && self.a_main_js && self.a_format_js;
        format!(
            "\n==================== 核心采集看板（输入 STATUS 可随时重看） ====================\n\
             [A 阶段源码]  HTML:[{}]  main.js:[{}]  format_check.js:[{}]{}\n\
             [业务实测单]  A-下段提交:[{}]  B-补打:[{}]  C-退回:[{}]  D-归班:[{}]\n\
             ================================================================================\n",
            ok(self.a_html),
            ok(self.a_main_js),
            ok(self.a_format_js),
            if a_ready { "  ★A源码已抓齐" } else { "" },
            cnt(self.a_submit_post),
            cnt(self.b_submit_post),
            cnt(self.c_submit_post),
            cnt(self.d_submit_post),
        )
    }
}

/// pointerdown 时刻的现场：DOM 是同步取到的，图来自录屏缓冲
#[derive(Debug, Clone)]
struct PointerCtx {
    seq: u64,
    css: Option<String>,
    id: Option<String>,
    #[allow(dead_code)]
    ts_ms: f64,
    before_png: Option<String>,
    before_html: Option<String>,
    method: String,
    at: std::time::Instant,
}

pub async fn run(cfg: AttachCfg) -> Result<i32> {
    let sess = Session::open(&cfg.out)?;
    if let Some(s) = cfg.step {
        sess.set_step(s);
    }

    // 热键（失败不致命：§7.5.4 要求主功能不受影响）
    match hotkeys::install(cfg.hotkeys.as_deref(), sess.clone()) {
        Ok(table) => hotkeys::report_effective(&sess, &table),
        Err(e) => println!("[WARN] 热键不可用：{e:#}\n        可用 --step 固定阶段，或每阶段重开一次探针。"),
    }

    println!("[attach] 连接 CDP 127.0.0.1:{} …", cfg.port);
    let (cdp, rx) = Cdp::connect(cfg.port).await?;
    if sess.resumed {
        println!("[attach] 续跑：继续写入上一次未完成的 session {}", sess.dir.display());
    } else {
        println!("[attach] 已连接。session 目录：{}", sess.dir.display());
    }
    println!("[attach] 操作员可以开始操作了。按热键切阶段，按结束热键（默认 Ctrl+Alt+F9）收尾。");
    println!("[attach] 热键不可用时，可在这个窗口里输入 A/B/C/D 回车切阶段，输入 END 回车结束记录。");

    let rec = Recorder::open(&sess.path("timeline.jsonl"))?;

    // ---- 数据入口（§5）：只读清单，绝不自动填写 ----
    let inputs = Inputs::load(cfg.numbers.as_deref(), cfg.values.as_deref())?;
    if !inputs.is_empty() {
        println!("{}", inputs.progress_line());
        let hint = inputs::values_hint(&inputs.values);
        if !hint.is_empty() {
            println!("[固定值] {hint}");
        }
        if !inputs.numbers.warnings.is_empty() {
            for w in &inputs.numbers.warnings {
                println!("[清单] {w}");
            }
        }
    }

    // ---- 语义标注（§6）----
    let annotator = if cfg.annotate {
        let a = Annotator::from_plan(cfg.plan.as_deref())?;
        println!("[标注] 已开启。每做一个动作前输入数字回车（输入 # 看 SOP 动作表，Ctrl+Alt+1..9 也行）");
        Some(a)
    } else {
        None
    };
    let annot_q: Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    for line in hotkeys::install_annot_keys(annot_q.clone()) {
        println!("  {line}");
    }

    // ---- 影印机 ----
    let mirror = Mirror::new(sess.path("mirror"), cfg.mirror_max_mb);
    // 只扒源码：不录像、不截操作前后图（省 CPU 和磁盘，操作员零按键）
    let film_enabled = !cfg.no_filmstrip && !cfg.mirror_only;
    let film = Filmstrip::new(
        sess.path("filmstrip"),
        film_enabled,
        cfg.full_page_shots,
        cfg.filmstrip_max_mb,
    );
    let (film_tx, film_rx) = tokio::sync::mpsc::unbounded_channel::<FilmAfter>();
    spawn_stdin_control(sess.clone(), annot_q.clone());

    let mut app = App {
        sess: sess.clone(),
        rec,
        cdp,
        net: NetStore::new(),
        corr: Correlator::new(),
        dialogs: DialogStore::new(),
        pages: HashMap::new(),
        jobs_seen: jhash(&win32::default_printer().map(|p| win32::jobs(&p)).unwrap_or_default()),
        tick: 0,
        step_for_snapshot: None,
        last_dom_snapshot: None,
        spool_wait: 0,
        mirror,
        film,
        inputs,
        annotator,
        annot_q: annot_q.clone(),
        film_tx,
        last_progress: std::time::Instant::now(),
        pending_scripts: std::collections::VecDeque::new(),
        harvester_done: HashSet::new(),
        screencast: crate::filmstrip::Screencast::default(),
        last_pointer: None,
        guide_flow: None,
        guide_idx: 0,
        pre_elements: Vec::new(),
        pre_collected: HashSet::new(),
        complete_harvested: HashSet::new(),
        mhtml_done: HashSet::new(),
        auto_done: false,
        auto_scan_ticks: 0,
        auto_wait_ticks: 0,
        auto_found: HashSet::new(),
        auto_unharvested: HashSet::new(),
        auto_last_url: String::new(),
        dom_done: HashSet::new(),
        pending_iframe_harvest: std::collections::VecDeque::new(),
        fallback_fetch_tries: HashMap::new(),
        last_index_flush: std::time::Instant::now(),
        last_index_fetched: 0,
        board: CoverageBoard::default(),
        cfg,
    };

    if app.cfg.auto_pages {
        println!("[自动巡页] 已开启：登录后会自动打开那 4 个业务页面（只读链接、只开标签页，不点任何按钮）。");
    }
    if app.cfg.mirror_only {
        println!("============================================================");
        println!(" 操作员只需三步：");
        println!("   1. 在打开的浏览器里登录");
        println!("   2. 登录后什么都不用做（工具会自己打开 4 个业务页面）");
        println!("   3. 看到「抓取完成……现在可以直接关掉浏览器了」就关浏览器");
        println!("------------------------------------------------------------");
        println!(" 如果提示「只找到 N/4」，把缺的页面手动点开一次，再关浏览器。");
        println!(" 黑窗口不要关。");
        println!("============================================================");
    }

    // ---- 引导模式（--guide）：一趟性任务的保险 ----
    if app.cfg.guide {
        let flow = crate::sopdiff::load_flow(app.cfg.plan.as_deref());
        let seq = crate::sopdiff::expected_sequence(&flow);
        println!("[引导] 已加载 SOP 期望流程：{} 个动作（来自 {}）", seq.len(), flow.source);
        if let Some((no, name, st)) = seq.first() {
            println!("[引导] 下一步 #{no} {name}（{st} 阶段）");
            println!("       如果实际点的是别的，我会记一条偏差。");
        }
        app.guide_flow = Some(seq);
        app.guide_idx = 0;
    }

    app.emit("attach_started", json!({"out": sess.dir.to_string_lossy()}))?;
    app.cdp.enable_auto_attach().await?;
    app.initial_targets().await?;

    app.run_loop(rx, film_rx).await
}

/// 热键的兜底通道：从 stdin 读阶段指令。
/// 内网机上热键被输入法/远程桌面占用时，这是唯一的切换手段（§7.5.4 第 3 条的延伸）。
fn spawn_stdin_control(
    sess: Arc<Session>,
    q: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(l) = line else { break };
            let cmd = l.trim().to_ascii_uppercase();
            match cmd.as_str() {
                "" => {}
                "A" | "B" | "C" | "D" => {
                    if let Some(st) = Step::from_letter(&cmd) {
                        sess.set_step(st);
                        println!("[stdin] 阶段 → {}（{}）", st.as_str(), st.cn());
                    }
                }
                "END" | "Q" | "QUIT" => {
                    println!("[stdin] 结束记录，正在生成产物…");
                    sess.request_stop();
                    break;
                }
                "#" => {
                    if let Ok(mut g) = q.lock() {
                        g.push_back("table".to_string());
                    }
                }
                "MIRROR" => {
                    if let Ok(mut g) = q.lock() {
                        g.push_back("mirror".to_string());
                    }
                }
                "STATUS" | "S" => {
                    if let Ok(mut g) = q.lock() {
                        g.push_back("status".to_string());
                    }
                }
                other if other.chars().all(|c| c.is_ascii_digit()) => {
                    if let Ok(mut g) = q.lock() {
                        g.push_back(format!("annot:{other}"));
                    }
                }
                _ => println!("[stdin] 可用指令：A/B/C/D 切阶段，数字 标记 SOP 动作，STATUS 看抓齐看板，# 看动作表，MIRROR 看镜像进度，END 结束"),
            }
        }
    });
}

fn jhash(jobs: &[win32::JobInfo]) -> HashSet<u32> {
    jobs.iter().map(|j| j.job_id).collect()
}

impl App {
    fn emit(&mut self, kind: &str, extra: Value) -> Result<u64> {
        self.rec.emit(&self.sess, kind, extra)
    }

    /// 已存在的标签页也要接管（auto-attach 只对新 target 生效）
    async fn initial_targets(&mut self) -> Result<()> {
        let targets = commands::get_targets(&self.cdp).await?;
        self.emit("targets_list", json!({"count": targets.len(), "targets": targets}))?;
        for t in &targets {
            let kind = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let attached = t.get("attached").and_then(|v| v.as_bool()).unwrap_or(false);
            if (kind == "page" || kind == "iframe") && !attached {
                if let Some(id) = t.get("targetId").and_then(|v| v.as_str()) {
                    let _ = self
                        .cdp
                        .call_browser("Target.attachToTarget", json!({"targetId": id, "flatten": true}))
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn run_loop(
        &mut self,
        mut rx: UnboundedReceiver<crate::cdp::events::Event>,
        mut film_rx: UnboundedReceiver<FilmAfter>,
    ) -> Result<i32> {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    match maybe {
                        Some(ev) => {
                            if ev.method == "__closed" {
                                println!("[attach] CDP 连接已断开（浏览器被关闭？）");
                                break;
                            }
                            if let Err(e) = self.handle(ev).await {
                                tracing::warn!("处理事件出错（继续）：{e:#}");
                                let _ = self.emit("error", json!({"module": "app", "message": format!("{e:#}")}));
                            }
                        }
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    if let Err(e) = self.on_tick().await {
                        tracing::warn!("tick 出错（继续）：{e:#}");
                    }
                }
                maybe_film = film_rx.recv() => {
                    if let Some(fa) = maybe_film {
                        // 延迟截的"后"图：用 ref_seq 与 click 事件对齐（§4.3）
                        let _ = self.rec.emit_seq(self.sess.next_seq(), &self.sess, "filmstrip_after", json!({
                            "ref_seq": fa.seq,
                            "after": fa.after,
                            "after_html": fa.after_html
                        }));
                    }
                }
            }
            if self.sess.stop_requested() {
                println!("[attach] 收到结束信号，收尾中…");
                break;
            }
        }

        // 收尾前等一等还在 spawn 的"后"图任务（最多 1.2 秒），别把最后几次操作的 after 丢了
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_millis(1200);
        while std::time::Instant::now() < drain_deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(250), film_rx.recv()).await {
                Ok(Some(fa)) => {
                    let _ = self.rec.emit_seq(self.sess.next_seq(), &self.sess, "filmstrip_after", json!({
                        "ref_seq": fa.seq,
                        "after": fa.after,
                        "after_html": fa.after_html
                    }));
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }

        self.finish().await
    }

    async fn on_tick(&mut self) -> Result<()> {
        self.tick += 1;

        // ---- 标注指令队列（stdin 数字 / Ctrl+Alt+1..9 / # / MIRROR）----
        let mut cmds: Vec<String> = Vec::new();
        if let Ok(mut g) = self.annot_q.lock() {
            while let Some(c) = g.pop_front() {
                cmds.push(c);
            }
        }
        for c in cmds {
            if let Some(rest) = c.strip_prefix("annot:") {
                let n: u32 = rest.parse().unwrap_or(0);
                match self.annotator.as_mut() {
                    Some(a) => match a.mark(n) {
                        Ok(msg) => println!("{msg}"),
                        Err(e) => println!("[标注] {e}"),
                    },
                    None => println!("[标注] 未开启 --annotate，本次不记录动作编号"),
                }
            } else if c == "table" {
                match &self.annotator {
                    Some(a) => println!("SOP 动作表：\n{}", a.table_text()),
                    None => println!("[标注] 未开启 --annotate"),
                }
            } else if c == "mirror" {
                println!("{}", self.mirror.summary_line());
                println!("{}", self.film.summary_line());
            } else if c == "status" {
                self.sync_board_from_mirror();
                println!("{}", self.board.format_panel());
                println!("{}", self.mirror.summary_line());
            }
        }
        if let Some(a) = self.annotator.as_mut() {
            if let Some(msg) = a.expire_if_needed() {
                println!("{msg}");
            }
        }

        // ---- 自动巡页：登录后自己把 4 个业务页面打开 ----
        if self.cfg.auto_pages && !self.auto_done {
            let want = crate::autopages::PAGES.len();
            // 登录可能要好几分钟。页面 URL 一变（登录成功跳转）就重新计时，
            // 否则操作员还在输验证码，这边已经"放弃"了（实测踩到过）。
            if self.tick % 8 == 0 {
                if let Some(sid) = self.active_page_session() {
                    let cur = self.pages.get(&sid).map(|p| p.url.clone()).unwrap_or_default();
                    if cur != self.auto_last_url {
                        self.auto_last_url = cur;
                        if self.auto_found.len() < want {
                            self.auto_scan_ticks = 0;
                        }
                    }
                }
            }
            if self.auto_scan_ticks < 150
                && self.auto_found.len() < want
                && self.tick % 8 == 0
            {
                self.auto_scan_ticks += 1;
                if let Some(sid) = self.active_page_session() {
                    let cur_url = self
                        .pages
                        .get(&sid)
                        .map(|p| p.url.clone())
                        .unwrap_or_default();
                    let origin = crate::autopages::origin_of(&cur_url);
                    if let Ok(v) = self.cdp.evaluate(&sid, crate::autopages::SCAN_JS).await {
                        let cands = crate::autopages::parse_candidates(&v);
                        let picks = crate::autopages::pick(&cands, &origin);
                        for (page, url) in picks {
                            if self.auto_found.contains(&page) {
                                continue;
                            }
                            if url == cur_url {
                                // 她已经打开这一页了
                                self.auto_found.insert(page.clone());
                                println!("[自动巡页] 当前页就是「{page}」，跳过");
                                continue;
                            }
                            let opened = self
                                .cdp
                                .call_browser(
                                    "Target.createTarget",
                                    json!({"url": url, "background": true}),
                                )
                                .await;
                            if opened.is_ok() {
                                self.auto_found.insert(page.clone());
                                self.auto_unharvested.insert(url.clone());
                                println!("[自动巡页] 已打开：{page} → {url}");
                            }
                        }
                    }
                }
                if self.auto_found.len() >= want && !self.auto_found.is_empty() {
                    println!("[自动巡页] 4 个业务页面已全部打开，正在抓取源码…");
                }
                if self.auto_scan_ticks >= 150 && self.auto_found.len() < want {
                    let missing: Vec<&str> = crate::autopages::PAGES
                        .iter()
                        .filter(|p| !self.auto_found.contains(**p))
                        .copied()
                        .collect();
                    println!(
                        "[自动巡页] 只找到 {}/4，没找到的请手动打开一次：{}",
                        self.auto_found.len(),
                        missing.join("、")
                    );
                    self.auto_done = true;
                }
            }
            if self.auto_found.len() >= want {
                self.auto_wait_ticks += 1;
                let all_done = self
                    .auto_unharvested
                    .iter()
                    .all(|u| self.complete_harvested.contains(u));
                if all_done || self.auto_wait_ticks > 160 {
                    println!(
                        "[自动巡页] 抓取完成（{} 个页面）。现在可以直接关掉浏览器了。",
                        self.auto_found.len()
                    );
                    self.auto_done = true;
                }
            }
        }

        // ---- 只扒源码：等页面真正加载完再抓（导航瞬间抓会漏资源）----
        if self.cfg.mirror_only {
            let candidate = self
                .pages
                .iter()
                .filter(|(_, p)| p.kind == "page")
                .map(|(sid, p)| (sid.clone(), p.url.clone()))
                .find(|(_, url)| {
                    !url.is_empty() && url != "about:blank" && !self.complete_harvested.contains(url)
                });
            if let Some((sid, url)) = candidate {
                let ready = self
                    .cdp
                    .evaluate(&sid, "document.readyState")
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s == "complete"))
                    .unwrap_or(false);
                if ready {
                    self.mirror_harvest(&sid).await;
                    self.complete_harvested.insert(url);
                    println!(
                        "[只扒源码] 已抓页面：{}",
                        self.pages.get(&sid).map(|p| p.title.clone()).unwrap_or_default()
                    );
                }
            }

            // 元素清单要独立预采集：资源在 frameNavigated 时就已经抓过了，
            // 不能拿 harvester_done 当采元素的开关，否则浏览器一关就什么都留不下。
            let el_candidate = self
                .pages
                .iter()
                .filter(|(_, p)| p.kind == "page")
                .map(|(sid, p)| (sid.clone(), p.url.clone()))
                .find(|(_, url)| {
                    !url.is_empty() && url != "about:blank" && !self.pre_collected.contains(url)
                });
            if let Some((sid, url)) = el_candidate {
                let ready = self
                    .cdp
                    .evaluate(&sid, "document.readyState")
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s == "complete"))
                    .unwrap_or(false);
                if ready {
                    let targets = commands::get_targets(&self.cdp).await.unwrap_or_default();
                    if let Ok(v) = domino::collect(&self.cdp, &sid, &targets).await {
                        self.pre_collected.insert(url);
                        self.pre_elements.push((sid.clone(), v));
                        println!(
                            "[只扒源码] 已存页面元素清单：{}",
                            self.pages.get(&sid).map(|p| p.title.clone()).unwrap_or_default()
                        );
                    }
                }
            }
        }

        // ---- 子 iframe 延迟补抓（等 iframe 真正加载完，再走三路兜底 + 同源 fetch）----
        if let Some(job) = self.pop_due_iframe_job() {
            self.harvest_iframe_assets(job).await;
        }

        // 运行时脚本按小批量抓（每次 tick 最多 16 个），有 URL 的业务脚本已排在队头
        let mut script_pulled = false;
        for _ in 0..16 {
            let Some((sid, script_id, url, parent)) = self.pending_scripts.pop_front() else {
                break;
            };
            let _ = self
                .mirror
                .add_runtime_script(&self.cdp, &sid, &script_id, &url, parent)
                .await;
            script_pulled = true;
        }
        if script_pulled {
            self.sync_board_from_mirror();
        }
        // 增量落盘 INDEX.json（即使误关黑窗口也不丢已抓源码索引）
        self.flush_index_if_needed(false);
        // 孤立的 pointerdown（被 preventDefault 取消、没有 click）2 秒后丢弃
        if let Some(pc) = &self.last_pointer {
            if pc.at.elapsed() > std::time::Duration::from_secs(2) {
                let seq = pc.seq;
                self.last_pointer = None;
                self.emit(
                    "note",
                    json!({"text": "pointer_down_without_click", "ref_seq": seq}),
                )?;
            }
        }

        if self.last_progress.elapsed() > std::time::Duration::from_secs(30) {
            self.last_progress = std::time::Instant::now();
            if !self.inputs.is_empty() {
                println!("{}", self.inputs.progress_line());
            }
        }

        // 热键切阶段
        if let Some(step) = self.sess.take_hotkey_step() {
            if self.step_for_snapshot != Some(step) {
                self.step_for_snapshot = Some(step);
                self.emit(
                    "note",
                    json!({"text": format!("hotkey_step:{} ({})", step.as_str(), step.cn())}),
                )?;
                self.dialogs.set_step(Some(step.as_str().to_string()));
                self.snapshot_dom("step").await;
                if !self.cfg.no_screenshot {
                    self.shot_active_page("step").await;
                }
                // 每进入一个业务阶段，存一份 MHTML（自包含，可离线打开）
                if let Some(sid) = self.active_page_session() {
                    let _ = self
                        .mirror
                        .capture_mhtml(&self.cdp, &sid, Some(step.as_str()))
                        .await;
                }
            }
        }

        // 500ms：原生窗口增量（§7.6、§7.8.2）
        if self.tick % 2 == 0 {
            let wins = win32::list_windows();
            let new = self.dialogs.diff_windows(&wins);
            for w in new {
                // PrintWindow 是同步 Win32 调用，可能卡在目标窗口的 UI 线程上，
                // 必须扔到阻塞线程并加超时，否则整个事件循环会被一个窗口拖死。
                let shot_rel = if self.cfg.no_screenshot {
                    None
                } else {
                    self.native_shot_timed(w.hwnd).await
                };
                let kind = crate::dialogs::classify_window(&w);
                let seq = self.dialogs.add_native(&w, shot_rel.clone());
                self.emit(
                    "dialog_native",
                    json!({
                        "dialog_seq": seq,
                        "hwnd": w.hwnd,
                        "class": w.class,
                        "title": w.title,
                        "pid": w.pid,
                        "exe": w.exe,
                        "rect": w.rect,
                        "kind": kind,
                        "screenshot": shot_rel,
                    }),
                )?;
            }
        }

        // 2s：打印 spooler 被动观测（§7.8.3）
        // 注意：现场默认打印机常是网络打印机，OpenPrinter/EnumJobs 在打印机不可达时
        // 会阻塞几十秒。必须带超时 + 退避，否则整个探针会被一次打印查询冻住。
        if self.spool_wait == 0 && self.tick % 8 == 0 {
            if let Some(printer) = win32::default_printer() {
                let p2 = printer.clone();
                let jobs = match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    tokio::task::spawn_blocking(move || win32::jobs(&p2)),
                )
                .await
                {
                    Ok(Ok(j)) => j,
                    Ok(Err(e)) => {
                        tracing::warn!("打印队列查询失败：{e}");
                        Vec::new()
                    }
                    Err(_) => {
                        self.spool_wait = 12; // 约 30 秒后再试
                        self.emit("error", json!({"module": "win32", "message": "打印队列查询超时（网络打印机不可达？），已降频到每 30 秒一次"}))?;
                        Vec::new()
                    }
                };
                for j in &jobs {
                    if !self.jobs_seen.contains(&j.job_id) {
                        self.jobs_seen.insert(j.job_id);
                        let ev = json!({
                            "ts": crate::util::now_iso(),
                            "printer": printer,
                            "job_id": j.job_id,
                            "document": j.document,
                            "total_pages": j.total_pages,
                            "status": j.status,
                            "step": self.sess.step().map(|s| s.as_str()),
                        });
                        append_jsonl(&self.sess.path("spool.jsonl"), &ev)?;
                        self.emit("note", json!({"text": "spooler_job", "detail": ev}))?;
                    }
                }
                // 已消失的 job 清掉，允许 jobId 复用
                self.jobs_seen.retain(|id| jobs.iter().any(|j| j.job_id == *id));
            }
        }
        if self.spool_wait > 0 {
            self.spool_wait -= 1;
        }

        Ok(())
    }

    async fn handle(&mut self, ev: crate::cdp::events::Event) -> Result<()> {
        let method = ev.method.clone();
        let session = ev.session_id.clone().unwrap_or_default();

        match method.as_str() {
            "Target.attachedToTarget" => {
                let Some(info) = ev.get("targetInfo").cloned() else { return Ok(()) };
                let kind = info.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let sid = ev.str("sessionId").unwrap_or("").to_string();
                if sid.is_empty() {
                    return Ok(());
                }
                if kind != "page" && kind != "iframe" {
                    return Ok(());
                }
                self.pages.insert(
                    sid.clone(),
                    PageRef {
                        target_id: info.get("targetId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        kind: kind.clone(),
                        url: info.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        title: info.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    },
                );
                self.cdp.enable_domains(&sid).await;
                if let Err(e) = self.cdp.add_binding(&sid, "__probe").await {
                    tracing::warn!("addBinding 失败：{e:#}");
                }
                if let Err(e) = domino::inject_probes(&self.cdp, &sid).await {
                    tracing::warn!("注入取证脚本失败：{e:#}");
                }
                self.emit(
                    "target_attached",
                    json!({
                        "target_id": info.get("targetId"),
                        "type": kind,
                        "url": info.get("url"),
                        "title": info.get("title"),
                        "session": sid,
                    }),
                )?;

                // ---- 影印机（PROMPT §3）----
                if !self.cfg.no_debugger {
                    if let Err(e) = commands::enable_debugger(&self.cdp, &sid, 50_000_000).await {
                        tracing::warn!("Debugger.enable 失败（跳过运行时脚本）：{e:#}");
                    }
                }
                // 固定视口：同一操作在不同机器上截出同样尺寸（§4.5）
                let _ = commands::set_device_metrics(&self.cdp, &sid, 1920, 1080, 1.0).await;

                // 录屏（修复 4b）：一直录，收到 pointerdown 时回头取帧。
                // --no-filmstrip 时不录，省 CPU。
                if !self.cfg.no_filmstrip && !self.cfg.mirror_only {
                    if let Err(e) = commands::start_screencast(&self.cdp, &sid).await {
                        tracing::warn!("Page.startScreencast 失败（回退为即时截图）：{e:#}");
                    }
                }
                self.mirror_harvest(&sid).await;

                if kind == "page" && !self.cfg.no_screenshot {
                    self.shot_active_page("attach").await;
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(sid) = ev.str("sessionId") {
                    self.pages.remove(sid);
                    self.emit("note", json!({"text": "target_detached", "session": sid}))?;
                }
            }
            "Network.requestWillBeSent" => {
                let seq = self.sess.next_seq();
                if let Some(idx) = self.net.on_request(&ev, seq, self.sess.mono_ms()) {
                    // 500ms 关联窗口（§7.5.3）
                    if let Some(click) = self.corr.attribute(self.sess.mono_ms()) {
                        let (cseq, ctext) = (click.seq, click.text.clone());
                        if let Some(r) = self.net.get_mut(&self.net.recs[idx].request_id.clone()) {
                            r.caused_by_click_seq = Some(cseq);
                            r.click_text = Some(ctext);
                        }
                    }
                    let req_url = self.net.recs[idx].url.clone();
                    let req_method = self.net.recs[idx].method.clone();
                    let req_nums = self.net.recs[idx].numbers_in_body;
                    if self.board.observe_request(&req_url, &req_method, req_nums) {
                        self.print_board();
                    }
                    let body = self.net.request_event(idx, self.cfg.include_cookies);
                    self.rec.emit_seq(seq, &self.sess, "request", body)?;
                }
            }
            "Network.requestWillBeSentExtraInfo" => self.net.on_extra_info(&ev),
            "Network.responseReceived" => {
                self.net.on_response(&ev);
            }
            "Network.loadingFinished" => {
                if let Some((idx, want_body)) = self.net.on_finished(&ev) {
                    if want_body {
                        let rid = self.net.recs[idx].request_id.clone();
                        match commands::get_response_body(&self.cdp, &session, &rid).await {
                            Ok((bytes, _b64)) => {
                                let binary = !looks_text(&self.net.recs[idx].mime);
                                // Document 全量落盘：业务页面只有 10-15KB，存下来供离线分析
                                if !binary && self.net.recs[idx].request_type == "Document" {
                                    let url = self.net.recs[idx].url.clone();
                                    let mime = self.net.recs[idx].mime.clone().unwrap_or_else(|| "text/html".to_string());
                                    let status = self.net.recs[idx].status;
                                    if let Some(rel) = save_page_html(&self.sess, &url, &bytes) {
                                        self.net.mark_body_file(idx, &rel);
                                    }
                                    // 同步用真实 Document 正文写入 mirror/（覆盖任何可能误存的 portal 外壳）
                                    let _ = self.mirror.store_raw(
                                        &url,
                                        "Document",
                                        &mime,
                                        &bytes,
                                        "network_document",
                                        status,
                                        true,
                                    );
                                    if url.to_ascii_lowercase().contains("/setroadseg/main")
                                        && bytes.len() > 500
                                    {
                                        let text = String::from_utf8_lossy(&bytes);
                                        if text.contains("itemid") || text.contains("setroadseg") || text.contains("投递下段") {
                                            if !self.board.a_html {
                                                self.board.a_html = true;
                                                self.print_board();
                                            }
                                            self.flush_index_if_needed(true);
                                        }
                                    }
                                }
                                self.net.set_body(idx, &bytes, binary);
                            }
                            Err(e) => self.net.mark_body_unavailable(idx, &format!("{e}")),
                        }
                    } else {
                        // 镜像需要 JS/CSS/图片/文档的正文（§3.2c 后备途径）
                        let rtype = self.net.recs[idx].request_type.clone();
                        let rid = self.net.recs[idx].request_id.clone();
                        let url = self.net.recs[idx].url.clone();
                        let mime = self.net.recs[idx].mime.clone().unwrap_or_default();
                        let status = self.net.recs[idx].status;
                        if matches!(
                            rtype.as_str(),
                            "Script" | "Stylesheet" | "Image" | "Font" | "Document"
                        ) {
                            let _ = self
                                .mirror
                                .fetch_by_request(&self.cdp, &session, &rid, &url, &rtype, &mime, status)
                                .await;
                            if url.contains("/delivery-web/") {
                                self.sync_board_from_mirror();
                                self.flush_index_if_needed(true);
                            }
                        }
                        self.net.mark_body_unavailable(idx, "url_only");
                    }
                    let body = self.net.response_event(idx);
                    self.rec.emit_seq(
                        self.sess.next_seq(),
                        &self.sess,
                        "response",
                        body,
                    )?;
                }
            }
            "Network.loadingFailed" => {
                if let Some(idx) = self.net.on_failed(&ev) {
                    let rid = self.net.recs[idx].request_id.clone();
                    self.emit(
                        "error",
                        json!({"module": "netcap", "request_id": rid, "message": ev.get("errorText")}),
                    )?;
                }
            }
            "Page.screencastFrame" => {
                let ts = ev
                    .get("metadata")
                    .and_then(|m| m.get("timestamp"))
                    .and_then(|t| t.as_f64())
                    .unwrap_or(0.0);
                if let Some(b64) = ev.str("data") {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                        self.screencast.push(ts, bytes);
                    }
                }
                if let Some(fid) = ev.get("sessionId").and_then(|v| v.as_i64()) {
                    let _ = self
                        .cdp
                        .call(
                            "Page.screencastFrameAck",
                            json!({"sessionId": fid}),
                            Some(&session),
                        )
                        .await;
                }
            }
            "Debugger.scriptParsed" => {
                // 运行时脚本（eval / 动态 <script>）：带 URL 的业务脚本插队头优先抓，
                // 避免被首页工作台几百个匿名内联脚本挤爆队列（修复上次 A 阶段 JS 丢失隐患）
                let script_id = ev.str("scriptId").unwrap_or("").to_string();
                let url = ev.str("url").unwrap_or("").to_string();
                if !script_id.is_empty() && !self.cfg.no_debugger {
                    let parent = self.pages.get(&session).map(|p| p.url.clone());
                    let is_priority = !url.is_empty()
                        && (url.contains("/delivery-web/") || !url.starts_with("chrome"));
                    if is_priority {
                        if !self.mirror.is_captured(&url) {
                            self.pending_scripts
                                .push_front((session.clone(), script_id, url, parent));
                        }
                    } else if self.pending_scripts.len() < 600 {
                        self.pending_scripts
                            .push_back((session.clone(), script_id, url, parent));
                    }
                }
            }
            "Runtime.bindingCalled" => {
                if let Some(payload) = ev.binding_payload() {
                    self.on_binding(payload, &session).await?;
                }
            }
            "Page.javascriptDialogOpening" => {
                let msg = ev.str("message").unwrap_or("").to_string();
                let dtype = ev.str("type").unwrap_or("alert").to_string();
                let seq = self.dialogs.add_js(Some(msg.clone()), Some(dtype.clone()));
                self.emit(
                    "dialog_js",
                    json!({"dialog_seq": seq, "message": msg, "dialog_type": dtype}),
                )?;
                // 必须立刻放行，否则页面冻结、操作员点不动（§7.6 实现顺序 1）
                if let Err(e) = commands::handle_js_dialog(&self.cdp, &session, true).await {
                    tracing::warn!("handleJavaScriptDialog 失败：{e:#}");
                }
            }
            "Page.frameNavigated" => {
                let frame = ev.get("frame");
                let url = frame
                    .and_then(|f| f.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let parent_id = frame
                    .and_then(|f| f.get("parentId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_subframe = !parent_id.is_empty();
                self.emit(
                    "note",
                    json!({
                        "text": "frame_navigated",
                        "url": truncate_bytes(&url, 300).to_string(),
                        "subframe": is_subframe
                    }),
                )?;
                // 重新注入（addScriptToEvaluateOnNewDocument 已覆盖，但当前文档要补一次）
                if let Err(e) = domino::inject_probes(&self.cdp, &session).await {
                    tracing::warn!("导航后重新注入失败：{e:#}");
                }

                if is_subframe {
                    // 子 iframe 导航绝不覆写顶层 page.url（修复：避免把外层 /portal/a 壳写进子页路径）
                    // 改为排队 1.5s 后等 iframe 加载完再做三路补抓 + 同源 fetch 兜底
                    if !url.is_empty()
                        && !url.starts_with("about:")
                        && !url.starts_with("javascript:")
                        && !url.starts_with("chrome")
                    {
                        if url.contains("/delivery-web/") {
                            println!("[监测] 业务子页已打开：{url}（1.5 秒后自动抓全量 DOM + JS 兜底）");
                        }
                        self.enqueue_iframe_harvest(session.clone(), url);
                    }
                } else {
                    // 顶层文档导航：更新 PageRef 并做镜像
                    if let Some(p) = self.pages.get_mut(&session) {
                        p.url = url.clone();
                    }
                    let is_page = self
                        .pages
                        .get(&session)
                        .map(|p| p.kind == "page")
                        .unwrap_or(false);
                    if is_page && !url.starts_with("about:") {
                        self.mirror_harvest(&session).await;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_binding(&mut self, p: Value, session: &str) -> Result<()> {
        let ty = p.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();

        if ty == "installed" {
            self.emit("note", json!({"text": "probe_js_installed", "url": p.get("url")}))?;
            return Ok(());
        }

        if ty == "mutation_dialog" {
            let rect = p.get("rect").and_then(|r| r.as_array()).map(|a| {
                [
                    a.first().and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                    a.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                    a.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                    a.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                ]
            });
            let selector = p.get("selector").and_then(|v| v.as_str()).map(|s| s.to_string());
            let text = p.get("text_preview").and_then(|v| v.as_str()).map(|s| s.to_string());
            let z = p.get("z_index").and_then(|v| v.as_i64()).map(|v| v as i32);
            let dseq = self.dialogs.add_dom(selector.clone(), text.clone(), rect, z);
            self.emit(
                "dialog_dom",
                json!({
                    "dialog_seq": dseq,
                    "selector": selector,
                    "text_preview": text,
                    "rect": rect,
                    "z_index": z,
                    "url": p.get("url"),
                }),
            )?;
            if !self.cfg.no_screenshot {
                self.shot_active_page("dialog").await;
            }
            return Ok(());
        }

        // ---- pointerdown：同步取 DOM + 从录屏缓冲取帧（修复 4）----
        if ty == "pointer_down" {
            self.on_pointer_down(&p).await?;
            return Ok(());
        }

        if !matches!(ty.as_str(), "click" | "change" | "enter") {
            return Ok(());
        }

        // ---- 1) seq 先定下来：快照文件名用它（六位零填充 = 字典序 = 时间序）----
        let seq = self.sess.next_seq();

        // ---- 2) "前"图优先用 pointerdown 那一刻的现场：
        //        DOM 是页面内同步取的，方向才是"点击前"；否则拍到的是"之后"。
        let mut action_seq = seq;
        let mut before = None;
        let mut before_html = None;
        let mut before_method = "late_best_effort".to_string();
        let mut pointer_used = false;

        if ty == "click" {
            if let Some(pc) = self.last_pointer.take() {
                let same = pointer_matches(&pc, &p);
                if same && pc.at.elapsed() <= std::time::Duration::from_secs(2) {
                    action_seq = pc.seq;
                    before = pc.before_png.clone();
                    before_html = pc.before_html.clone();
                    before_method = pc.method.clone();
                    pointer_used = true;
                } else {
                    // 元素对不上 = 孤立的 pointerdown，记一笔但不认领
                    let _ = self.emit(
                        "note",
                        json!({"text": "pointer_down_without_click", "ref_seq": pc.seq}),
                    );
                }
            }
        }

        if !pointer_used {
            let film = self.film.before(&self.cdp, session, action_seq).await;
            before = film.before;
            before_html = film.before_html;
            before_method = if self.cfg.no_filmstrip { "disabled" } else { "late_best_effort" }.to_string();
        }

        let mut body = json!({
            "element": p.get("element"),
            "url": p.get("url"),
            "filmstrip": {
                "before": before,
                "before_html": before_html,
                "before_method": before_method
            },
        });
        if pointer_used {
            body["ref_seq"] = json!(action_seq);
        }
        if ty == "click" {
            body["target_chain"] = p.get("target_chain").cloned().unwrap_or(Value::Null);
        } else {
            body["value_len"] = p.get("value_len").cloned().unwrap_or(Value::Null);
            body["value_preview"] = p.get("value_preview").cloned().unwrap_or(Value::Null);
            body["value_hash"] = p.get("value_hash").cloned().unwrap_or(Value::Null);
            body["value_masked"] = p.get("value_masked").cloned().unwrap_or(Value::Null);
            body["tokens"] = p.get("tokens").cloned().unwrap_or(json!([]));
        }

        // ---- 3) 语义标注（若有）----
        // 只让 click/enter 消费标注：change 是"打字"，真实系统里打字频繁，
        // 让它吃掉"下一个操作"会把标注全打到输入框上（二次 trial 实测到的）。
        if ty != "change" {
            if let Some(a) = self.annotator.as_mut() {
                if let Some(ann) = a.take_for_operation(seq) {
                let mut ev = ann.clone();
                ev["ref_seq"] = json!(seq);
                let _ = self
                    .rec
                    .emit_seq(self.sess.next_seq(), &self.sess, "annotation", ev);
                    body["annotation"] = ann;
                }
            }
        }

        // ---- 4) 落 timeline ----
        self.rec.emit_seq(seq, &self.sess, &ty, body)?;

        // ---- 5) 点击：关联窗口 + 弹窗触发点 ----
        if ty == "click" {
            let text = p
                .get("element")
                .and_then(|e| e.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let css = p
                .get("element")
                .and_then(|e| e.get("css"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            self.corr
                .note_click(seq, text.clone(), css, self.sess.mono_ms());
            self.dialogs.note_click(seq, text);
        }

        // ---- 5.5) 引导模式：与期望动作比对，只提示、不改行为 ----
        if self.cfg.guide {
            let shown = p
                .get("element")
                .and_then(|e| e.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            self.guide_check(shown);
        }

        // ---- 6) change：单号识别 / 固定值确认（§5.4）----
        if ty == "change" {
            self.on_change_values(seq, &p)?;
        }

        // ---- 7) 400ms 后截"后"图（spawn，不阻塞事件循环）----
        self.film.schedule_after(
            self.cdp.clone(),
            session.to_string(),
            action_seq,
            400,
            self.film_tx.clone(),
        );

        Ok(())
    }

    /// 引导校验：与期望序列比对，命中就前进，不命中就提示偏差（只打印）
    fn guide_check(&mut self, shown: &str) {
        let Some(flow) = self.guide_flow.clone() else { return };
        let Some((no, name, step)) = flow.get(self.guide_idx).cloned() else { return };
        let score = crate::sopdiff::similarity(shown, &name);
        if score >= 0.6 {
            self.guide_idx += 1;
            match flow.get(self.guide_idx) {
                Some((n2, name2, st2)) => {
                    println!("[引导] 下一步 #{n2} {name2}（{st2} 阶段）");
                }
                None => println!("[引导] 文档里的动作已全部走完，后面是自由操作"),
            }
        } else if !shown.trim().is_empty() {
            println!("[引导] 偏差：#{} 期望「{}」，实际点了「{}」", no, name, shown);
        }
        let _ = step;
    }

    /// pointerdown：同步 DOM 直接落盘；图从录屏环形缓冲里取
    async fn on_pointer_down(&mut self, p: &Value) -> Result<()> {
        let seq = self.sess.next_seq();
        let element = p.get("element").cloned().unwrap_or(Value::Null);
        let css = element.get("css").and_then(|v| v.as_str()).map(|s| s.to_string());
        let id = element.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        let ts_ms = p.get("ts_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let (before_html, before_png, method) = if self.cfg.no_filmstrip {
            (None, None, "disabled".to_string())
        } else {
            let html = p
                .get("dom_html")
                .and_then(|v| v.as_str())
                .and_then(|h| self.film.save_dom_html(seq, "before", h));
            match self.screencast.frame_at_or_before(ts_ms / 1000.0) {
                Some(jpeg) => (
                    html,
                    self.film.save_jpeg_frame(seq, "before", jpeg),
                    "screencast".to_string(),
                ),
                // 缓冲里还没有 <= 该时刻的帧（刚启动/环境不支持）：交给 click 侧做即时截图
                None => (html, None, "none".to_string()),
            }
        };

        self.rec.emit_seq(
            seq,
            &self.sess,
            "pointer_down",
            json!({
                "element": element,
                "ts_ms": ts_ms,
                "dom_truncated": p.get("dom_truncated"),
                "filmstrip": {
                    "before": before_png,
                    "before_html": before_html,
                    "before_method": method
                }
            }),
        )?;

        self.last_pointer = Some(PointerCtx {
            seq,
            css,
            id,
            ts_ms,
            before_png,
            before_html,
            method,
            at: std::time::Instant::now(),
        });
        Ok(())
    }

    /// 输入框值的识别（§5.4）：原始值永不离开页面，这里只拿 token 和指纹
    fn on_change_values(&mut self, seq: u64, p: &Value) -> Result<()> {
        let field = p
            .get("element")
            .and_then(|e| e.get("css"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(tokens) = p.get("tokens").and_then(|t| t.as_array()) {
            for t in tokens {
                let tok = t.as_str().unwrap_or("").to_string();
                if tok.is_empty() {
                    continue;
                }
                match self.inputs.mark_used(&tok) {
                    Some(item) => {
                        self.emit(
                            "input_number",
                            json!({
                                "hash8": item.hash8,
                                "masked": item.masked,
                                "idx": item.idx,
                                "field": field,
                                "ref_seq": seq
                            }),
                        )?;
                    }
                    None => {
                        if !self.inputs.numbers.numbers.is_empty() {
                            self.emit(
                                "input_unknown",
                                json!({
                                    "masked": inputs::mask_number(&tok),
                                    "field": field,
                                    "ref_seq": seq,
                                    "reason": "不在 --numbers 清单里"
                                }),
                            )?;
                        }
                    }
                }
            }
        }

        if let Some(vh) = p.get("value_hash").and_then(|v| v.as_str()) {
            if !self.inputs.values.map.is_empty() {
                let matched = self
                    .inputs
                    .values
                    .map
                    .iter()
                    .find(|(_, v)| inputs::fnv32_hex(v) == vh)
                    .map(|(k, _)| k.clone());
                match matched {
                    Some(k) => {
                        self.emit(
                            "input_confirmed",
                            json!({"key": k, "field": field, "ref_seq": seq}),
                        )?;
                    }
                    None => {
                        let label = p
                            .get("element")
                            .and_then(|e| e.get("placeholder"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if label.contains("手机") {
                            self.emit(
                                "warn",
                                json!({
                                    "module": "inputs",
                                    "message": "手机号与 --values 配置不符",
                                    "actual_masked": p.get("value_masked"),
                                    "ref_seq": seq
                                }),
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 抓"主文档 + 同源 iframe"的完整 DOM，落到 `pages/`。
    /// 为什么要连 iframe：这系统的业务表单都在 iframe 里，只存主文档等于没存。
    async fn capture_full_dom(&mut self, sid: &str) {
        const JS: &str = r#"(function(){
          try {
            var out = [];
            function walk(doc, name, depth) {
              if (depth > 3) return;
              try {
                var html = doc.documentElement ? doc.documentElement.outerHTML : '';
                out.push({name: name, url: (doc.location ? doc.location.href : ''), html: html.slice(0, 3000000)});
                var ifr = doc.querySelectorAll('iframe');
                for (var i = 0; i < ifr.length && i < 20; i++) {
                  try {
                    var cd = ifr[i].contentDocument;
                    if (cd && cd.documentElement) walk(cd, name + '-frame' + (i + 1), depth + 1);
                  } catch (e) {}
                }
              } catch (e) {}
            }
            walk(document, '', 0);
            return JSON.stringify(out);
          } catch (e) { return '[]'; }
        })()"#;

        let Ok(v) = self.cdp.evaluate(sid, JS).await else { return };
        let docs: Vec<Value> = serde_json::from_str(v.as_str().unwrap_or("[]")).unwrap_or_default();
        if docs.is_empty() {
            return;
        }
        let Ok(dir) = self.sess.ensure_subdir("pages") else { return };
        let mut n = 0;
        let mut board_changed = false;
        for d in &docs {
            let html = d.get("html").and_then(|h| h.as_str()).unwrap_or("");
            if html.trim().len() < 32 {
                continue;
            }
            let doc_url = d.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let seq = self.sess.next_seq();
            let name = d.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let fname = format!("dom-{seq:06}{name}.html");
            if std::fs::write(dir.join(&fname), html.as_bytes()).is_ok() {
                n += 1;
                let _ = self.emit(
                    "note",
                    json!({
                        "text": "page_dom",
                        "path": format!("pages/{fname}"),
                        "url": doc_url,
                        "bytes": html.len()
                    }),
                );
            }
            // 若该子 iframe 是 A 阶段（setroadseg/main）且包含表单控件，点亮看板 A-HTML
            if doc_url.to_ascii_lowercase().contains("/setroadseg/main")
                && (html.contains("itemid") || html.contains("btnAdd") || html.contains("btnroadseg"))
            {
                if !self.board.a_html {
                    self.board.a_html = true;
                    board_changed = true;
                }
                // 若 mirror 里尚未存下更权威的 network_document，先把这份真实子页 DOM 兜底落盘
                if !self.mirror.is_captured(doc_url) {
                    let _ = self.mirror.store_raw(
                        doc_url,
                        "Document",
                        "text/html",
                        html.as_bytes(),
                        "iframe_dom_outerHTML",
                        Some(200),
                        false,
                    );
                }
            }
        }
        if n > 0 {
            println!("[页面] 已存 {} 份页面 DOM（含 iframe）", n);
        }
        if board_changed {
            self.print_board();
        }
    }

    fn enqueue_iframe_harvest(&mut self, sid: String, url: String) {
        // 同 URL 已在队列里则更新等待时间，避免重复堆积
        let next_at = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        if let Some(existing) = self
            .pending_iframe_harvest
            .iter_mut()
            .find(|j| j.sid == sid && j.url == url)
        {
            existing.next_at = next_at;
            existing.retries = 0;
            return;
        }
        if self.pending_iframe_harvest.len() < 40 {
            self.pending_iframe_harvest.push_back(IframeHarvestJob {
                sid,
                url,
                next_at,
                retries: 0,
            });
        }
    }

    fn pop_due_iframe_job(&mut self) -> Option<IframeHarvestJob> {
        let now = std::time::Instant::now();
        let pos = self
            .pending_iframe_harvest
            .iter()
            .position(|j| j.next_at <= now)?;
        self.pending_iframe_harvest.remove(pos)
    }

    /// 子 iframe 加载完成后执行完整补抓：readyState 检测 → 资源树补漏 → 完整 DOM → 同源 fetch 兜底缺失 JS → 刷盘 INDEX/elements
    async fn harvest_iframe_assets(&mut self, mut job: IframeHarvestJob) {
        if !self.pages.contains_key(&job.sid) {
            return;
        }
        // 1) 检查目标子 iframe 是否已经加载完成（最多等 ~5 秒）
        let check_js = format!(
            r#"(function(targetUrl){{
              try {{
                var ready = null;
                function walk(doc, depth) {{
                  if (depth > 3 || ready === 'complete') return;
                  var ifs = doc.querySelectorAll('iframe');
                  for (var i = 0; i < ifs.length && i < 20; i++) {{
                    try {{
                      var cd = ifs[i].contentDocument;
                      if (!cd) continue;
                      var u = cd.location ? cd.location.href : '';
                      if (u && (u === targetUrl || u.indexOf(targetUrl.split('?')[0]) === 0)) {{
                        ready = cd.readyState || 'loading';
                        return;
                      }}
                      walk(cd, depth + 1);
                    }} catch (e) {{}}
                  }}
                }}
                walk(document, 0);
                return ready || 'missing';
              }} catch (e) {{ return 'err'; }}
            }})({})"#,
            serde_json::to_string(&job.url).unwrap_or_else(|_| "\"\"".to_string())
        );
        let state = self
            .cdp
            .evaluate(&job.sid, &check_js)
            .await
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "err".to_string());
        if state != "complete" && job.retries < 6 {
            job.retries += 1;
            job.next_at = std::time::Instant::now() + std::time::Duration::from_millis(600);
            self.pending_iframe_harvest.push_back(job);
            return;
        }

        // 2) 资源树补漏（此时 iframe 已加载完，且 mirror.rs 不会因之前失败占坑）
        if let Ok(list) = self.mirror.walk_resource_tree(&self.cdp, &job.sid).await {
            for (url, fid_kind) in list {
                let (fid, kind) = match fid_kind.split_once('|') {
                    Some(x) => x,
                    None => ("", "Other"),
                };
                if !fid.is_empty() {
                    let _ = self
                        .mirror
                        .fetch_by_content(&self.cdp, &job.sid, fid, &url, kind, "")
                        .await;
                }
            }
        }

        // 3) 抓全量主文档 + 所有同源 iframe DOM
        self.capture_full_dom(&job.sid).await;

        // 4) 同源主动兜底：专门针对 /delivery-web/ 缺失的 JS/HTML，用 cache:'force-cache' 只读拉取
        self.fallback_fetch_iframe_scripts(&job.sid, &job.url).await;

        // 5) 同步看板、刷盘 INDEX.json，并在业务页打开后预采集一份 elements.json 防误关黑窗口
        self.sync_board_from_mirror();
        self.flush_index_if_needed(true);
        if job.url.contains("/delivery-web/") {
            let targets = commands::get_targets(&self.cdp).await.unwrap_or_default();
            if let Ok(v) = domino::collect(&self.cdp, &job.sid, &targets).await {
                self.pre_elements.retain(|(s, _)| s != &job.sid);
                self.pre_elements.push((job.sid.clone(), v));
                let merged = domino::merge(self.pre_elements.clone());
                let _ = std::fs::write(
                    self.sess.path("elements.json"),
                    serde_json::to_vec_pretty(&merged).unwrap_or_default(),
                );
            }
        }
    }

    /// 核武器兜底：在同源页面内扫描所有同源 iframe 的 `<script src>`，
    /// 对尚未落盘到 mirror/ 的核心业务 JS（优先 `/delivery-web/`）用只读 `fetch(..., {cache:'force-cache'})` 补齐。
    async fn fallback_fetch_iframe_scripts(&mut self, sid: &str, iframe_url: &str) {
        const COLLECT_SCRIPTS_JS: &str = r#"(function(){
          try {
            var urls = [];
            function walk(doc, depth) {
              if (depth > 3) return;
              try {
                var base = (doc.location && doc.location.href) ? doc.location.href : location.href;
                var scs = doc.querySelectorAll('script[src]');
                for (var i = 0; i < scs.length; i++) {
                  var raw = scs[i].getAttribute('src') || '';
                  if (!raw) continue;
                  try {
                    var abs = new URL(raw, base).href;
                    if (abs.indexOf(location.origin) === 0 && urls.indexOf(abs) < 0) {
                      urls.push(abs);
                    }
                  } catch (e) {}
                }
                var ifs = doc.querySelectorAll('iframe');
                for (var j = 0; j < ifs.length && j < 20; j++) {
                  try {
                    var cd = ifs[j].contentDocument;
                    if (cd) walk(cd, depth + 1);
                  } catch (e) {}
                }
              } catch (e) {}
            }
            walk(document, 0);
            return JSON.stringify(urls);
          } catch (e) { return '[]'; }
        })()"#;

        let Ok(v) = self.cdp.evaluate(sid, COLLECT_SCRIPTS_JS).await else { return };
        let mut urls: Vec<String> = serde_json::from_str(v.as_str().unwrap_or("[]")).unwrap_or_default();

        // 若目标业务子页原始 HTML 本身还没进 pages/mirror，也加入只读缓存兜底（不覆盖已有文件）
        if iframe_url.contains("/delivery-web/") && !self.mirror.is_captured(iframe_url) {
            urls.insert(0, iframe_url.to_string());
        }

        // 只兜底业务关键路径与缺失的脚本，优先处理 /delivery-web/
        let mut missing: Vec<String> = urls
            .into_iter()
            .filter(|u| {
                if self.mirror.is_captured(u) {
                    return false;
                }
                let tries = self.fallback_fetch_tries.get(u).copied().unwrap_or(0);
                if tries >= 2 {
                    return false;
                }
                let low = u.to_ascii_lowercase();
                low.contains("/delivery-web/")
                    || low.contains("/setroadseg/")
                    || low.contains("/waybillprint/")
                    || low.contains("/signwaybillreturned/")
                    || low.contains("/returnwork/")
            })
            .collect();
        // 把 setroadseg / 业务 js 排最前面
        missing.sort_by_key(|u| {
            let low = u.to_ascii_lowercase();
            if low.contains("setroadseg") {
                0
            } else if low.contains("/delivery/js/") {
                1
            } else {
                2
            }
        });
        missing.truncate(24);

        let mut recovered = 0usize;
        for u in missing {
            *self.fallback_fetch_tries.entry(u.clone()).or_insert(0) += 1;
            let js = format!(
                r#"(async function(u){{
                  try {{
                    var resp = await fetch(u, {{ credentials: 'include', cache: 'force-cache' }});
                    if (!resp || !resp.ok) return '';
                    var txt = await resp.text();
                    return txt.slice(0, 2000000);
                  }} catch (e) {{ return ''; }}
                }})({})"#,
                serde_json::to_string(&u).unwrap_or_else(|_| "\"\"".to_string())
            );
            let Ok(res) = self.cdp.evaluate(sid, &js).await else { continue };
            let text = res.as_str().unwrap_or("");
            if text.trim().len() < 16 {
                continue;
            }
            let is_doc = u == iframe_url && !u.to_ascii_lowercase().contains(".js");
            let (kind, mime) = if is_doc {
                ("Document", "text/html")
            } else {
                ("Script", "application/javascript")
            };
            if let Ok(true) = self.mirror.store_raw(
                &u,
                kind,
                mime,
                text.as_bytes(),
                "same_origin_fetch",
                Some(200),
                false,
            ) {
                recovered += 1;
                if is_doc {
                    let _ = save_page_html(&self.sess, &u, text.as_bytes());
                }
                let _ = self.emit(
                    "note",
                    json!({
                        "text": "mirror_fallback_recovered",
                        "url": u,
                        "bytes": text.len()
                    }),
                );
            }
        }
        if recovered > 0 {
            println!("[兜底补抓] 同源只读补齐 {recovered} 个缺失业务文件（含 JS/HTML）");
        }
    }

    /// 增量刷盘 INDEX.json：有新资源且超过 3 秒，或强制刷新时落盘
    fn flush_index_if_needed(&mut self, force: bool) {
        let cur = self.mirror.fetched;
        if cur == 0 {
            return;
        }
        let changed = cur != self.last_index_fetched;
        let elapsed = self.last_index_flush.elapsed() >= std::time::Duration::from_secs(3);
        if (force && changed) || (changed && elapsed) {
            if self.mirror.write_index().is_ok() {
                self.last_index_flush = std::time::Instant::now();
                self.last_index_fetched = cur;
            }
        }
    }

    /// 从 mirror 落盘状态同步 A 阶段看板，有新增点亮时立即打印
    fn sync_board_from_mirror(&mut self) {
        let mut changed = false;
        if !self.board.a_main_js && self.mirror.has_saved_suffix("setroadseg/main.js") {
            self.board.a_main_js = true;
            changed = true;
        }
        if !self.board.a_format_js && self.mirror.has_saved_suffix("setroadseg/format_check.js") {
            self.board.a_format_js = true;
            changed = true;
        }
        if changed {
            self.print_board();
        }
    }

    fn print_board(&mut self) {
        self.board.printed_once = true;
        println!("{}", self.board.format_panel());
    }

    /// 当前最像业务页的 page session（截图 / MHTML 用）
    fn active_page_session(&self) -> Option<String> {
        self.pages
            .iter()
            .find(|(_, p)| p.kind == "page" && !p.url.starts_with("devtools://") && p.url != "about:blank")
            .or_else(|| self.pages.iter().find(|(_, p)| p.kind == "page"))
            .map(|(k, _)| k.clone())
    }

    /// 抓这个 target 的全部资源 + 一份 MHTML（§3.2b/a）
    async fn mirror_harvest(&mut self, sid: &str) {
        let page_url = self
            .pages
            .get(sid)
            .map(|p| p.url.clone())
            .unwrap_or_else(|| "about:blank".to_string());
        // 注意：这里**不能**因为"抓过一次"就 return。
        // 页面可能还在加载时就被抓过（比如操作员先开了页面、探针后连上），
        // 等 readyState 变成 complete 后必须能再抓一遍补漏。
        // 重复抓的代价由 Mirror 内部的 seen_urls / mhtml_done 去重兜住。
        self.harvester_done.insert(page_url.clone());
        let saved_html = self
            .mirror
            .save_document(&self.cdp, sid, &page_url)
            .await
            .unwrap_or(None);

        match self.mirror.walk_resource_tree(&self.cdp, sid).await {
            Ok(list) => {
                let n = list.len();
                for (url, fid_kind) in list {
                    let (fid, kind) = match fid_kind.split_once('|') {
                        Some(x) => x,
                        None => ("", "Other"),
                    };
                    if fid.is_empty() {
                        continue;
                    }
                    let _ = self
                        .mirror
                        .fetch_by_content(&self.cdp, sid, fid, &url, kind, "")
                        .await;
                }
                if n > 0 {
                    println!("{}", self.mirror.summary_line());
                }
            }
            Err(e) => {
                let _ = self.emit(
                    "error",
                    json!({"module": "mirror", "message": format!("getResourceTree 失败：{e}")}),
                );
            }
        }
        if self.mhtml_done.insert(page_url.clone()) {
            let step = self.sess.step().map(|s| s.as_str().to_string());
            let _ = self.mirror.capture_mhtml(&self.cdp, sid, step.as_deref()).await;
        }
        // 业务页面常常在 iframe 里，光存主文档没有表单；这里连 iframe 一起存
        if self.dom_done.insert(page_url.clone()) {
            self.capture_full_dom(sid).await;
        }
        self.sync_board_from_mirror();
        self.flush_index_if_needed(false);
        if self.cfg.no_debugger {
            // Debugger 关闭时的兜底：从刚保存的页面 HTML 里抠内联 <script>（§3.2e）
            if let Some(html) = saved_html {
                self.mirror.extract_inline_scripts(&html, &page_url);
            }
        }
    }

    /// 原生窗口截图（带超时保护）
    async fn native_shot_timed(&self, hwnd: isize) -> Option<String> {
        let sess = self.sess.clone();
        let handle = tokio::task::spawn_blocking(move || shot::native_shot(hwnd, &sess, "dialog"));
        match tokio::time::timeout(std::time::Duration::from_secs(3), handle).await {
            Ok(Ok(Some((p, _w, _h, m)))) => Some(format!("{}#{}", shot::rel(&self.sess, &p), m)),
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                tracing::warn!("原生截图任务失败：{e}");
                None
            }
            Err(_) => {
                tracing::warn!("原生截图超时（hwnd={hwnd}），跳过");
                None
            }
        }
    }

    /// 选一个"最像业务页"的 page 截图
    async fn shot_active_page(&mut self, tag: &str) {
        if let Some((sid, _)) = self
            .pages
            .iter()
            .find(|(_, p)| p.kind == "page" && !p.url.starts_with("devtools://") && p.url != "about:blank")
            .or_else(|| self.pages.iter().find(|(_, p)| p.kind == "page"))
        {
            let sid = sid.clone();
            match shot::page_shot(&self.cdp, &sid, &self.sess, tag).await {
                Ok(Some(p)) => {
                    let rel = shot::rel(&self.sess, &p);
                    let _ = self.emit("screenshot", json!({"path": rel, "method": "page"}));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("页面截图失败：{e:#}");
                }
            }
        }
    }

    /// DOM 快照（§4.2 的 snapshots/），同一 step 只存一次
    async fn snapshot_dom(&mut self, reason: &str) {
        let Some((sid, _)) = self.pages.iter().find(|(_, p)| p.kind == "page") else {
            return;
        };
        let sid = sid.clone();
        let Ok(v) = self
            .cdp
            .evaluate(&sid, "document.documentElement ? document.documentElement.outerHTML : ''")
            .await
        else {
            return;
        };
        let Some(html) = v.as_str() else { return };
        let redacted = crate::redact::redact_text(&truncate_bytes(html, 2 * 1024 * 1024).to_string());
        if self.last_dom_snapshot.as_deref() == Some(redacted.as_str()) {
            return;
        }
        self.last_dom_snapshot = Some(redacted.clone());
        let dir = match self.sess.ensure_subdir("snapshots") {
            Ok(d) => d,
            Err(_) => return,
        };
        let step = self.sess.step().map(|s| s.as_str().to_string()).unwrap_or_else(|| "X".into());
        let name = format!(
            "{step}-{}-{reason}.html",
            time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                .unix_timestamp()
        );
        let path = dir.join(name);
        if std::fs::write(&path, redacted).is_ok() {
            let rel = shot::rel(&self.sess, &path);
            let _ = self.emit("note", json!({"text": "dom_snapshot", "path": rel}));
        }
    }

    async fn finish(&mut self) -> Result<i32> {
        // 1) 元素清单（每个 page session 采一次，合并）
        let targets = commands::get_targets(&self.cdp).await.unwrap_or_default();
        let sids: Vec<String> = self
            .pages
            .iter()
            .filter(|(_, p)| p.kind == "page" || p.kind == "iframe")
            .map(|(k, _)| k.clone())
            .collect();

        // 收尾再抓一次完整 DOM（有些页面是最后才渲染出来的）
        for sid in &sids {
            self.capture_full_dom(sid).await;
        }

        let mut parts = Vec::new();
        for sid in &sids {
            // 保险：先补注入一次元素采集脚本（导航过、或偶发丢失时也能采到）
            if let Err(e) = self.cdp.evaluate(sid, domino::INSPECT_JS).await {
                tracing::warn!("补注入 inspect_dom 失败 {sid}: {e:#}");
            }
            match domino::collect(&self.cdp, sid, &targets).await {
                Ok(v) => parts.push((sid.clone(), v)),
                Err(e) => {
                    println!("[WARN] 元素采集失败 {sid}: {e:#}");
                    let _ = self.emit(
                        "error",
                        json!({"module": "domino", "session": sid, "message": format!("{e:#}")}),
                    );
                }
            }
        }
        if parts.is_empty() && !self.pre_elements.is_empty() {
            // 浏览器已经被关了：用会话中提前采到的那份
            parts = self.pre_elements.clone();
            println!("[元素] 浏览器已关闭，使用会话中预采集的 {} 个页面元素清单", parts.len());
        }
        if parts.is_empty() {
            println!("[WARN] 没有任何 page/iframe session 可用，elements.json 会是空的（原因已写进报告）");
        }
        let elements = if parts.is_empty() {
            json!({"generated_at": crate::util::now_iso(), "page": {"elements_truncated": false},
                   "frames": [], "control_map": {}, "error": "未获取到（原因：没有可用的 page session）"})
        } else {
            domino::merge(parts)
        };
        self.emit("note", json!({"text": "elements_collected"}))?;

        // ---- 影印机收尾：先把队列里残留的带 URL 业务脚本快速捞完，再写 INDEX.json ----
        let mut drained = 0usize;
        while drained < 32 {
            let Some((sid, script_id, url, parent)) = self.pending_scripts.pop_front() else {
                break;
            };
            if url.is_empty() {
                continue;
            }
            let _ = self
                .mirror
                .add_runtime_script(&self.cdp, &sid, &script_id, &url, parent)
                .await;
            drained += 1;
        }
        self.sync_board_from_mirror();
        let _ = self.mirror.write_index();
        let _ = self.inputs.write_json(&self.sess.path("inputs.json"));
        if let Some(a) = &self.annotator {
            let _ = a.write_csv(&self.sess.path("annotations.csv"));
        }
        println!("{}", self.board.format_panel());
        println!("{}", self.mirror.summary_line());
        println!("{}", self.film.summary_line());
        if !self.inputs.is_empty() {
            println!("{}", self.inputs.progress_line());
        }
        let _ = self.emit(
            "mirror_summary",
            json!({
                "resources": self.mirror.fetched,
                "skipped": self.mirror.skipped,
                "inline": self.mirror.inline.len(),
                "mhtml": self.mirror.mhtml.len(),
                "bytes": self.mirror.total_bytes,
                "shots": self.film.shots_count(),
                "doms": self.film.doms.load(std::sync::atomic::Ordering::Relaxed),
            }),
        );

        // 2) Q2 判定：静态 H1 + 网络 H2
        let static_inputs = match sids.first() {
            Some(sid) => experiment::static_check(&self.cdp, sid).await.unwrap_or(json!([])),
            None => json!([]),
        };
        let h1 = experiment::h1_verdict(&static_inputs);
        let h2 = experiment::h2_verdict(&self.net);
        let exp = experiment::build(static_inputs, h1, h2, None);

        // 3) 报告
        let edge_version = Cdp::version(self.cfg.port)
            .await
            .ok()
            .and_then(|v| v.get("Browser").and_then(|b| b.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "未获取到".to_string());
        let preflight = crate::preflight::load(&self.sess.root);

        // 收尾时再拍一张全局截图
        if !self.cfg.no_screenshot {
            self.shot_active_page("final").await;
        }

        let input = report::Input {
            sess: &self.sess,
            net: &self.net,
            dialogs: &self.dialogs,
            elements: &elements,
            experiment: &exp,
            preflight,
            edge_version,
            include_cookies: self.cfg.include_cookies,
        };
        report::write_all(&input).with_context(|| "生成报告失败")?;
        crate::session::mark_finished(&self.sess);

        let md = self.sess.path("report.md");
        println!("\n[完成] 产物目录：{}", self.sess.dir.display());
        println!("[完成] 报告：{}", md.display());
        println!(
            "[完成] Q2 判定：{}；请求 {} 条；弹窗 {} 个",
            exp["q2_verdict"].as_str().unwrap_or("unknown"),
            self.net.recs.len(),
            self.dialogs.dialogs.len()
        );
        Ok(0)
    }
}

/// pointerdown 与随后的 click 是否指向同一个元素
fn pointer_matches(pc: &PointerCtx, click: &Value) -> bool {
    let el = click.get("element").cloned().unwrap_or(Value::Null);
    let css = el.get("css").and_then(|v| v.as_str());
    let id = el.get("id").and_then(|v| v.as_str());
    if let (Some(a), Some(b)) = (&pc.id, id) {
        if !a.is_empty() && a == b {
            return true;
        }
    }
    if let (Some(a), Some(b)) = (&pc.css, css) {
        if !a.is_empty() && a == b {
            return true;
        }
    }
    false
}

/// 把 Document 的完整正文存到 `pages/<host>/<path>.html`，返回相对 session 的路径。
/// 为什么单独存：业务页 HTML 里有表单控件和 inline 脚本，是离线写自动化的第一手材料。
fn save_page_html(sess: &Session, url: &str, bytes: &[u8]) -> Option<String> {
    let mut rel = crate::mirror::url_to_path(url)?;
    if rel.extension().is_none() {
        rel.set_extension("html");
    }
    let path = sess.path("pages").join(&rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&path, bytes).ok()?;
    Some(format!("pages/{}", rel.to_string_lossy().replace('\\', "/")))
}

fn append_jsonl(path: &std::path::Path, v: &Value) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(v.to_string().as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()?;
    Ok(())
}

fn looks_text(mime: &Option<String>) -> bool {
    match mime {
        Some(m) => {
            let m = m.to_ascii_lowercase();
            m.contains("text/") || m.contains("json") || m.contains("xml") || m.contains("javascript")
        }
        None => true,
    }
}

pub fn not_implemented(what: &str) -> anyhow::Error {
    anyhow!("{what} 尚未实现")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_ignores_init_calls_and_catches_real_submits() {
        let mut b = CoverageBoard::default();
        // A 阶段初始化请求不应误报为已提交
        assert!(!b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/setroadseg/getunfinishlist",
            "POST",
            0
        ));
        assert!(!b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/setroadseg/getselftakeorg",
            "GET",
            0
        ));
        assert_eq!(b.a_submit_post, 0);

        // A 阶段真实提交 POST 必须点亮
        assert!(b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/setroadseg/addwaybill",
            "POST",
            1
        ));
        assert_eq!(b.a_submit_post, 1);

        // B/C/D 阶段提交点亮
        assert!(b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/waybillprint/updatemobile",
            "POST",
            1
        ));
        assert!(b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/signwaybillreturned/completeconfirmBatch",
            "POST",
            1
        ));
        assert!(b.observe_request(
            "https://10.4.188.1/delivery-web/a/delivery/returnwork/acceptdelivertask",
            "POST",
            1
        ));

        b.a_html = true;
        b.a_main_js = true;
        b.a_format_js = true;
        let panel = b.format_panel();
        assert!(panel.contains("★A源码已抓齐"));
        assert!(panel.contains("A-下段提交:[OK(1条)]"));
    }
}
