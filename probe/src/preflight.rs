//! 环境自检（§7.2）。
//!
//! 全部检查项写进 `<out>/preflight.json`，stdout 用 `[OK]/[WARN]/[FAIL]` 人读格式。
//! 任一 **阻断项** 失败 → 退出码 1。

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::cli::PreflightArgs;
use crate::http1;
use crate::session::dir_writable;
use crate::win32;
use crate::util::now_iso;

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Check {
        Check { name, status: Status::Ok, detail: detail.into() }
    }
    fn warn(name: &'static str, detail: impl Into<String>) -> Check {
        Check { name, status: Status::Warn, detail: detail.into() }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Check {
        Check { name, status: Status::Fail, detail: detail.into() }
    }
    fn tag(&self) -> &'static str {
        match self.status {
            Status::Ok => "[OK]",
            Status::Warn => "[WARN]",
            Status::Fail => "[FAIL]",
        }
    }
}

pub async fn run(args: &PreflightArgs) -> Result<i32> {
    let mut checks: Vec<Check> = Vec::new();

    // 1. Edge 可执行文件
    let edge = win32::find_edge();
    match &edge {
        Some(p) => checks.push(Check::ok("Edge 可执行文件存在", p.display().to_string())),
        None => checks.push(Check::fail(
            "Edge 可执行文件存在",
            format!(
                "两个候选路径都不存在：{}",
                win32::edge_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        )),
    }

    // 2. Edge 版本（仅记录）
    let ver = win32::reg_string(r"SOFTWARE\Microsoft\Edge\BLBeacon", "version");
    checks.push(match &ver {
        Some(v) => Check::ok("Edge 版本", v.clone()),
        None => Check::warn("Edge 版本", "注册表未读到 BLBeacon.version，不影响运行"),
    });

    // 3. 调试端口
    // 注意：preflight 通常在启动 Edge 之前跑，此时端口没监听是正常的，
    // 所以这里升级为 WARN 而不是 FAIL（阻断项留给"端口被别的进程占"）。
    match http1::get_text(args.port, "/json/version", 2000).await {
        Ok(_t) => checks.push(Check::ok("调试端口可连", format!("127.0.0.1:{} 已就绪", args.port))),
        Err(_) => checks.push(Check::warn(
            "调试端口可连",
            format!(
                "127.0.0.1:{} 暂未监听。若尚未启动 Edge（用 启动Edge.bat 启动）属正常；\
                 若已启动仍不通，检查是否加了独立 --user-data-dir",
                args.port
            ),
        )),
    }

    // 4. 企业策略（阻断）
    let rda = win32::reg_dword(
        r"SOFTWARE\Policies\Microsoft\Edge",
        "RemoteDebuggingAllowed",
    );
    let dta = win32::reg_dword(
        r"SOFTWARE\Policies\Microsoft\Edge",
        "DeveloperToolsAvailability",
    );
    let policy_blocked = matches!(rda, Some(0)) || matches!(dta, Some(2));
    checks.push(if policy_blocked {
        Check::fail(
            "企业策略未禁用远程调试",
            format!(
                "RemoteDebuggingAllowed={:?} DeveloperToolsAvailability={:?} —— 被组策略禁用，请联系 IT，探针无法工作",
                rda, dta
            ),
        )
    } else {
        Check::ok(
            "企业策略未禁用远程调试",
            format!(
                "RemoteDebuggingAllowed={:?} DeveloperToolsAvailability={:?}",
                rda, dta
            ),
        )
    });

    // 5. 已有 Edge 实例
    let msedge_pids = win32::process_pids("msedge.exe");
    checks.push(if msedge_pids.is_empty() {
        Check::ok("当前是否已有 Edge 实例", "没有正在运行的 msedge.exe")
    } else {
        Check::warn(
            "当前是否已有 Edge 实例",
            format!(
                "已有 {} 个 msedge.exe 进程（PID {:?}）。launch 模式会强制使用独立 profile；\
                 也正因为有实例，直接双击 Edge 不会拿调试端口",
                msedge_pids.len(),
                &msedge_pids[..msedge_pids.len().min(5)]
            ),
        )
    });

    // 6. 输出目录可写（阻断）
    match dir_writable(&args.out) {
        Ok(()) => checks.push(Check::ok("输出目录可写", args.out.display().to_string())),
        Err(e) => checks.push(Check::fail("输出目录可写", format!("{e:#}"))),
    }

    // 7. 是否管理员（仅记录）
    checks.push(if win32::is_elevated() {
        Check::ok("是否管理员运行", "是（非必需）")
    } else {
        Check::ok("是否管理员运行", "否（非必需）")
    });

    // 8/9. 打印机
    let def = win32::default_printer();
    checks.push(match &def {
        Some(p) => Check::ok(
            "默认打印机",
            format!("{}（虚拟打印机：{}）", p, if win32::is_virtual_printer(p) { "是" } else { "否" }),
        ),
        None => Check::warn("默认打印机", "未取到默认打印机；Q7 将记为未获取到"),
    });
    let printers = win32::printers();
    checks.push(if printers.is_empty() {
        Check::warn("打印机列表", "枚举为空")
    } else {
        Check::ok("打印机列表", format!("{} 台：{}", printers.len(), printers.join(" | ")))
    });

    // 10. DPI
    let dpi = win32::dpi();
    checks.push(Check::ok("系统 DPI", format!("{dpi}")));

    // 11. 磁盘
    let free = win32::disk_free_bytes(&args.out);
    checks.push(match free {
        Some(b) => {
            let gb = b as f64 / (1024.0 * 1024.0 * 1024.0);
            if gb < 2.0 {
                Check::warn("磁盘剩余空间", format!("{gb:.2} GB，低于 2 GB，截图可能失败"))
            } else {
                Check::ok("磁盘剩余空间", format!("{gb:.1} GB"))
            }
        }
        None => Check::warn("磁盘剩余空间", "查询失败"),
    });

    // ---- 输出 ----
    println!("退货流程探针 · 环境自检");
    println!("时间：{}", now_iso());
    println!("输出目录：{}", args.out.display());
    println!("{}", "-".repeat(60));
    for c in &checks {
        println!("{} {} —— {}", c.tag(), c.name, c.detail);
    }
    println!("{}", "-".repeat(60));

    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();

    // preflight.json
    let json_val = json!({
        "generated_at": now_iso(),
        "port": args.port,
        "out": args.out.display().to_string(),
        "all_passed": fails == 0,
        "fails": fails,
        "warns": warns,
        "checks": checks.iter().map(|c| json!({
            "name": c.name,
            "status": c.tag().trim_matches(|c| c == '[' || c == ']'),
            "ok": c.status != Status::Fail,
            "detail": c.detail,
        })).collect::<Vec<_>>(),
    });
    std::fs::create_dir_all(&args.out).ok();
    let p = args.out.join("preflight.json");
    std::fs::write(&p, serde_json::to_vec_pretty(&json_val)?)?;

    if fails == 0 {
        println!("结论：全部通过{}。下一步：双击 启动Edge.bat 启动浏览器。",
            if warns > 0 { format!("（{warns} 项警告，见上）") } else { String::new() });
        println!("preflight.json → {}", p.display());
        Ok(0)
    } else {
        println!("结论：{fails} 项阻断，请先解决再继续（截图发给技术）。");
        println!("preflight.json → {}", p.display());
        Ok(1)
    }
}

/// 供 report 读取（mochi: 报告里带一条 preflight 结论更可信）
pub fn load(out: &PathBuf) -> Option<Value> {
    let s = std::fs::read_to_string(out.join("preflight.json")).ok()?;
    serde_json::from_str(&s).ok()
}
