//! 入口：CLI 分发。
//!
//! 退出码约定（§6）：
//! - 0 正常
//! - 1 阻断性失败（preflight 未通过、一般错误）
//! - 2 参数/前置条件拒绝执行（如 experiment 缺 --confirm-dry-run）
//! - 3 单实例锁被占用

use anyhow::Result;
use clap::Parser;

use probe::app::{self, AttachCfg};
use probe::cli::{Cli, Cmd};
use probe::launch;
use probe::report;
use probe::session;
use probe::session::Session;
use probe::win32;
use probe::{experiment_replay, mask, preflight};

fn main() {
    win32::console_utf8();

    let cli = Cli::parse();

    // 崩溃钩子必须尽早装；`--out` 只有部分命令有
    let panic_out = match &cli.cmd {
        Cmd::Preflight(a) => a.out.clone(),
        Cmd::Launch(a) => a.out.clone(),
        Cmd::Attach(a) => a.out.clone(),
        Cmd::Experiment(a) => a.out.clone(),
        Cmd::Report(a) => a.out.clone(),
        Cmd::Mask(a) => a.out.clone(),
        Cmd::Annotate(a) => a.out.clone(),
        Cmd::MirrorScan(a) => a.out.clone(),
        Cmd::Agentpack(a) => a.out.clone(),
        Cmd::Sopdiff(a) => a.out.clone(),
    };
    session::install_panic_hook(panic_out);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[FATAL] 初始化异步运行时失败：{e}");
            std::process::exit(1);
        }
    };

    let code = match rt.block_on(dispatch(cli)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[FATAL] {e:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Preflight(args) => preflight::run(&args).await,
        Cmd::Launch(args) => {
            // 拉不起 Edge 也不该让探针崩：报错并返回 1
            let launched = launch::launch_edge(args.edge.clone(), args.port, args.profile.clone()).await?;
            println!("[launch] Edge PID {}，转入 attach 观测。", launched.child.id());
            // 子进程句柄故意泄漏到进程结束：操作员还要继续用这个浏览器（§7.1-4）
            std::mem::forget(launched.child);
            app::run(AttachCfg {
                out: args.out,
                port: args.port,
                include_cookies: args.include_cookies,
                no_screenshot: args.no_screenshot,
                step: args.step,
                hotkeys: args.hotkeys,
                numbers: args.numbers,
                values: args.values,
                annotate: args.annotate,
                plan: args.plan,
                no_filmstrip: args.no_filmstrip,
                full_page_shots: args.full_page_shots,
                no_debugger: args.no_debugger,
                mirror_max_mb: args.mirror_max_mb,
                filmstrip_max_mb: args.filmstrip_max_mb,
                guide: args.guide,
                mirror_only: args.mirror_only,
                auto_pages: args.auto_pages,
            })
            .await
        }
        Cmd::Attach(args) => {
            app::run(AttachCfg {
                out: args.out,
                port: args.port,
                include_cookies: args.include_cookies,
                no_screenshot: args.no_screenshot,
                step: args.step,
                hotkeys: args.hotkeys,
                numbers: args.numbers,
                values: args.values,
                annotate: args.annotate,
                plan: args.plan,
                no_filmstrip: args.no_filmstrip,
                full_page_shots: args.full_page_shots,
                no_debugger: args.no_debugger,
                mirror_max_mb: args.mirror_max_mb,
                filmstrip_max_mb: args.filmstrip_max_mb,
                guide: args.guide,
                mirror_only: args.mirror_only,
                auto_pages: args.auto_pages,
            })
            .await
        }
        Cmd::Experiment(args) => experiment_replay::run(&args).await,
        Cmd::Report(args) => {
            // 从已有 timeline 重建报告（崩溃后补救，§6.5）
            let dir = probe::sopdiff::resolve_session(&args.out)?;
            println!("[report] 从 {} 重建报告", dir.display());
            report::rebuild(&dir, &args.out)
        }
        Cmd::Mask(args) => {
            let dir = probe::sopdiff::resolve_session(&args.out)?;
            mask::run(&dir)
        }
        Cmd::Annotate(args) => {
            let dir = probe::sopdiff::resolve_session(&args.out)?;
            let n = probe::annotate::merge_csv_into_timeline(
                &dir,
                &args.file,
                args.plan.as_deref(),
            )?;
            println!("[annotate] 合并了 {n} 条标注 → {}", dir.join("timeline.jsonl").display());
            println!("[annotate] 提示：用 `probe agentpack --out <out> --dest <PACK>` 重新编译语料包");
            Ok(0)
        }
        Cmd::MirrorScan(args) => {
            let sess = Session::open(&args.out)?;
            let dir = sess.dir.join("mirror");
            if !dir.is_dir() {
                println!("[mirror-scan] 没有 mirror/ 目录（先跑一次 attach）");
                return Ok(2);
            }
            let md = probe::mirror::scan_for_credentials(&dir)?;
            let p = dir.join("FINDINGS.md");
            std::fs::write(&p, md)?;
            println!("[mirror-scan] 结果：{}", p.display());
            Ok(0)
        }
        Cmd::Sopdiff(args) => {
            // 注意：这里**不要** Session::open —— 那会新建一个空的 session 目录
            let dir = probe::sopdiff::resolve_session(&args.out)?;
            let md = probe::sopdiff::run(&dir, args.plan.as_deref())?;
            println!("[sopdiff] 已写入 {}", dir.join("SOP-DIFF.md").display());
            if md.contains("低") {
                println!("[sopdiff] ⚠️ 标注率偏低，结论置信度低（重跑时按数字键标注）");
            }
            Ok(0)
        }
        Cmd::Agentpack(args) => {
            let pages = args.pages.as_ref().map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_ascii_uppercase())
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
            });
            let sum = probe::agentpack::build(&probe::agentpack::PackOptions {
                src: args.out,
                dest: args.dest,
                pages,
            })?;
            println!(
                "[agentpack] 完成：{} 个动作 / {} 个定位器 / {} 条请求 / {} 个页面 / {} 个缺口 / L1 {:.1} KB",
                sum.actions,
                sum.selectors,
                sum.requests,
                sum.pages,
                sum.unresolved,
                sum.l1_bytes as f64 / 1024.0
            );
            Ok(0)
        }
    }
}
