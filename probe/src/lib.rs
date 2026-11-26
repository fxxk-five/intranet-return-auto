//! 退货流程取证探针。
//!
//! 本 crate 的唯一目标是回答 `探针设计方案-Rust-v1.md` §2 的 Q1–Q7，
//! 产出可打包回内网外传的 `report.md` / `report.json` 与证据文件。
//!
//! 模块与问题的对应关系：
//! - `preflight`  → 前置条件（决定探针能不能跑）
//! - `cdp`        → CDP 通道（Q1/Q3/Q4 的底座）
//! - `netcap`     → Q1 请求取证、Q2-H2 判定
//! - `domino`     → Q3 定位器、Q4 标签页结构
//! - `correlate`  → 点击↔请求关联（让报告可读）
//! - `dialogs`    → Q5 弹窗、§9 弹窗清单
//! - `experiment` → Q2 的 H1/H2/H3 判定
//! - `shot`/`win32` → Q5/Q7 的原生窗口与打印证据
//! - `report`     → §9 报告
//! - `redact`     → §10 脱敏

pub mod agentpack;
pub mod annotate;
pub mod autopages;
pub mod app;
pub mod cdp;
pub mod cli;
pub mod correlate;
pub mod dialogs;
pub mod domino;
pub mod filmstrip;
pub mod inputs;
pub mod mirror;
pub mod experiment;
pub mod experiment_replay;
pub mod hotkeys;
pub mod http1;
pub mod launch;
pub mod mask;
pub mod netcap;
pub mod preflight;
pub mod recorder;
pub mod redact;
pub mod report;
pub mod session;
pub mod shot;
pub mod sopdiff;
pub mod util;
pub mod win32;
