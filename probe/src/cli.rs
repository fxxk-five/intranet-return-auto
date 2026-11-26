//! CLI 定义（§6）。参数名是外部契约，不要随手改名。

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "probe",
    version,
    about = "退货流程取证探针 —— 只观测，不干预业务",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 环境自检（§7.2）
    Preflight(PreflightArgs),
    /// 探针拉起 Edge 并观测（§7.1）
    Launch(LaunchArgs),
    /// 挂到已运行的 Edge（主用模式，§6.3）
    Attach(AttachArgs),
    /// Q2 阈值实验（会发真实请求，§7.7）
    Experiment(ExperimentArgs),
    /// 从已有事件流重新生成报告（§6.5）
    Report(ReportArgs),
    /// 单独对某个目录做脱敏（§6.6）
    Mask(MaskArgs),
    /// 把事后标注的 annotations.csv 合并进 timeline（§6.2 模式 B）
    Annotate(AnnotateArgs),
    /// 只读扫描 mirror 里的疑似凭据，产出 FINDINGS.md（§3.4）
    MirrorScan(MirrorScanArgs),
    /// 把原始数据编译成 AGENT_PACK（§6A，只在开发机跑）
    Agentpack(AgentpackArgs),
    /// SOP 一致性校验（改 plan.json 后可单独重跑）
    Sopdiff(SopdiffArgs),
}

#[derive(Args, Debug)]
pub struct PreflightArgs {
    /// 输出目录，preflight.json 写到这里
    #[arg(long, default_value = "out")]
    pub out: PathBuf,
    /// 调试端口
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
}

#[derive(Args, Debug)]
pub struct LaunchArgs {
    #[arg(long)]
    pub out: PathBuf,
    /// Edge 可执行文件；缺省自动探测两个标准路径
    #[arg(long)]
    pub edge: Option<PathBuf>,
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
    /// 独立 profile 目录；缺省 %LOCALAPPDATA%\probe\profile
    #[arg(long)]
    pub profile: Option<PathBuf>,
    /// 启动后直接进入的阶段
    #[arg(long, value_enum)]
    pub step: Option<Step>,
    /// 覆盖默认热键表，见 §7.5.4
    #[arg(long)]
    pub hotkeys: Option<String>,
    /// 单号清单（txt/csv，一行一个；用于"单号↔操作"关联）
    #[arg(long)]
    pub numbers: Option<PathBuf>,
    /// 固定值（JSON 或 key=value），用于确认操作员填的是配置里那个
    #[arg(long)]
    pub values: Option<PathBuf>,
    /// 开启 SOP 动作语义标注（stdin 输入数字 / Ctrl+Alt+1..9）
    #[arg(long)]
    pub annotate: bool,
    /// SOP 动作表覆盖（plan.json）
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// 引导模式：每步提示"下一步该做什么"，点错当场提示（不改变任何业务行为）
    #[arg(long)]
    pub guide: bool,
    /// 只扒源码：不录像、不标注、不引导，只把站点源码和页面搬回来（操作员零按键）
    #[arg(long = "mirror-only")]
    pub mirror_only: bool,
    /// 自动巡页：登录后由工具自己把那 4 个业务页面打开（只读链接、只开标签页，不点按钮）
    #[arg(long = "auto-pages")]
    pub auto_pages: bool,
    /// 关闭操作快照带（前后截图 + 完整 DOM）
    #[arg(long = "no-filmstrip")]
    pub no_filmstrip: bool,
    /// 整页截图（默认只截视口）
    #[arg(long = "full-page-shots")]
    pub full_page_shots: bool,
    /// 关闭 Debugger（不抓运行时脚本；页面慢时也能自动关）
    #[arg(long = "no-debugger")]
    pub no_debugger: bool,
    /// 源码镜像体积上限（MB）
    #[arg(long = "mirror-max-mb", default_value_t = 300)]
    pub mirror_max_mb: u64,
    /// 快照带体积上限（MB）
    #[arg(long = "filmstrip-max-mb", default_value_t = 500)]
    pub filmstrip_max_mb: u64,
    #[arg(long)]
    pub no_screenshot: bool,
    #[arg(long)]
    pub include_cookies: bool,
}

#[derive(Args, Debug)]
pub struct AttachArgs {
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
    /// 当前操作员正在走的阶段
    #[arg(long, value_enum)]
    pub step: Option<Step>,
    /// 覆盖默认热键表，见 §7.5.4
    #[arg(long)]
    pub hotkeys: Option<String>,
    /// 单号清单（txt/csv，一行一个；用于"单号↔操作"关联）
    #[arg(long)]
    pub numbers: Option<PathBuf>,
    /// 固定值（JSON 或 key=value），用于确认操作员填的是配置里那个
    #[arg(long)]
    pub values: Option<PathBuf>,
    /// 开启 SOP 动作语义标注（stdin 输入数字 / Ctrl+Alt+1..9）
    #[arg(long)]
    pub annotate: bool,
    /// SOP 动作表覆盖（plan.json）
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// 引导模式：每步提示"下一步该做什么"
    #[arg(long)]
    pub guide: bool,
    /// 只扒源码：零按键模式，操作员只要登录 + 打开页面 + 关浏览器
    #[arg(long = "mirror-only")]
    pub mirror_only: bool,
    /// 自动巡页：登录后由工具自己把 4 个业务页面打开
    #[arg(long = "auto-pages")]
    pub auto_pages: bool,
    /// 关闭操作快照带（前后截图 + 完整 DOM）
    #[arg(long = "no-filmstrip")]
    pub no_filmstrip: bool,
    /// 整页截图（默认只截视口）
    #[arg(long = "full-page-shots")]
    pub full_page_shots: bool,
    /// 关闭 Debugger（不抓运行时脚本；页面慢时也能自动关）
    #[arg(long = "no-debugger")]
    pub no_debugger: bool,
    /// 源码镜像体积上限（MB）
    #[arg(long = "mirror-max-mb", default_value_t = 300)]
    pub mirror_max_mb: u64,
    /// 快照带体积上限（MB）
    #[arg(long = "filmstrip-max-mb", default_value_t = 500)]
    pub filmstrip_max_mb: u64,
    #[arg(long)]
    pub no_screenshot: bool,
    #[arg(long)]
    pub include_cookies: bool,
}

#[derive(Args, Debug)]
pub struct ExperimentArgs {
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
    /// 捕获到的请求模板对应的 requestId
    #[arg(long = "request-id")]
    pub request_id: Option<String>,
    /// 要试的 N 值，逗号分隔
    #[arg(long, default_value = "21,22,30")]
    pub numbers: String,
    /// 必须显式加，否则退出码 2
    #[arg(long = "confirm-dry-run")]
    pub confirm_dry_run: bool,
    /// 实验用测试单号（会替换 body 里的真实单号）
    #[arg(long = "test-number")]
    pub test_number: Option<String>,
}

#[derive(Args, Debug)]
pub struct AnnotateArgs {
    #[arg(long)]
    pub out: PathBuf,
    /// annotations.csv（seq,动作编号,备注）
    #[arg(long)]
    pub file: PathBuf,
    /// SOP 动作表覆盖
    #[arg(long)]
    pub plan: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct MirrorScanArgs {
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(Args, Debug)]
pub struct AgentpackArgs {
    /// 原始 out 目录（或某个 session 目录）
    #[arg(long)]
    pub out: PathBuf,
    /// AGENT_PACK 输出目录
    #[arg(long)]
    pub dest: PathBuf,
    /// 只处理这些阶段，例：A,B,C,D
    #[arg(long)]
    pub pages: Option<String>,
}

#[derive(Args, Debug)]
pub struct SopdiffArgs {
    #[arg(long)]
    pub out: PathBuf,
    /// SOP 期望流程（plan.json 的 sop_flow）
    #[arg(long)]
    pub plan: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ReportArgs {
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(Args, Debug)]
pub struct MaskArgs {
    #[arg(long)]
    pub out: PathBuf,
}

/// SOP 四个阶段。END 只用于热键终止记录，不是一个业务阶段。
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Step {
    A,
    B,
    C,
    D,
}

impl Step {
    pub fn as_str(&self) -> &'static str {
        match self {
            Step::A => "A",
            Step::B => "B",
            Step::C => "C",
            Step::D => "D",
        }
    }

    /// SOP 里的中文阶段名（报告和热键提示用）
    pub fn cn(&self) -> &'static str {
        match self {
            Step::A => "下段扫描",
            Step::B => "详情单补打",
            Step::C => "退回寄件人/妥投确认",
            Step::D => "投递归班交接",
        }
    }

    pub fn from_letter(s: &str) -> Option<Step> {
        match s.trim().to_ascii_uppercase().as_str() {
            "A" => Some(Step::A),
            "B" => Some(Step::B),
            "C" => Some(Step::C),
            "D" => Some(Step::D),
            _ => None,
        }
    }
}
