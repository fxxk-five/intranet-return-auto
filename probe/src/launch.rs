//! `launch` 模式：探针自己拉起 Edge 并观测（§7.1）。
//!
//! 关键点（每条都踩过）：
//! - `--user-data-dir` 必须是独立目录，否则已有 Edge 实例会把新进程"吃掉"，9222 永远不监听
//! - `--remote-allow-origins=*` 必加，否则 Chromium 111+ 对 WebSocket 返回 403
//! - 用 `Command::new` 而不是 ShellExecute；子进程句柄保留，**退出时不强杀**

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Child;

use crate::http1;
use crate::win32;

pub struct Launched {
    pub child: Child,
    pub port: u16,
    pub profile: PathBuf,
}

fn default_profile() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\".to_string());
    PathBuf::from(base).join("probe").join("profile")
}

pub async fn launch_edge(
    edge: Option<PathBuf>,
    port: u16,
    profile: Option<PathBuf>,
) -> Result<Launched> {
    let edge = edge
        .or_else(win32::find_edge)
        .ok_or_else(|| anyhow::anyhow!("找不到 msedge.exe（试过两个标准路径），请用 --edge 指定"))?;
    let profile = profile.unwrap_or_else(default_profile);
    std::fs::create_dir_all(&profile)
        .with_context(|| format!("创建 profile 目录失败：{}", profile.display()))?;

    let args = vec![
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=*".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-features=msEdgeSidebarV2".to_string(),
        "--start-maximized".to_string(),
        "about:blank".to_string(),
    ];

    println!("[launch] {}", edge.display());
    for a in &args {
        println!("         {a}");
    }
    println!(
        "[launch] profile 目录等同于账号凭据，跑完不要把 {} 拷贝出去。",
        profile.display()
    );

    let child = std::process::Command::new(&edge)
        .args(&args)
        .spawn()
        .with_context(|| format!("启动 Edge 失败：{}", edge.display()))?;

    // 轮询 /json/version，最多 30s，每 250ms 一次（§7.1-3）
    let started = std::time::Instant::now();
    loop {
        if http1::get_text(port, "/json/version", 1000).await.is_ok() {
            println!("[launch] 调试端口已就绪：127.0.0.1:{port}");
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let pids = win32::process_pids("msedge.exe");
            bail!(
                "30 秒内 127.0.0.1:{port} 未就绪。常见原因：\n\
                 1) 本机已有 Edge 在运行且没有用独立 --user-data-dir（本次已用 {}，请先全部退出再试）\n\
                 2) 企业策略禁止远程调试（跑 probe preflight 看第 4 项）\n\
                 当前 msedge.exe 进程：{:?}",
                profile.display(),
                pids
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    Ok(Launched {
        child,
        port,
        profile,
    })
}
