//! `timeline.jsonl` 写入器（§8.1、§7.9-3）。
//!
//! 每行写完立刻 `flush` + `sync_data`：宁可慢，不许丢。

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use crate::session::Session;
use crate::util::now_iso;

/// 最后 50 行，崩溃钩子用（§7.9-5）
static LAST_LINES: LazyLock<Mutex<VecDeque<String>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

pub fn last_lines() -> Vec<String> {
    LAST_LINES
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default()
}

pub struct Recorder {
    file: Option<File>,
    path: std::path::PathBuf,
    /// 续跑时已有行数，seq 从它继续
    pub existing_lines: usize,
}

impl Recorder {
    /// 追加打开（断点续跑：同一 session 目录重开时接着写）。
    pub fn open(path: &Path) -> Result<Recorder> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let existing_lines = std::fs::read_to_string(path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("打开 timeline 失败：{}", path.display()))?;
        Ok(Recorder {
            file: Some(file),
            path: path.to_path_buf(),
            existing_lines,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 写一条事件，返回它拿到的 seq。`extra` 里不要重复放公共字段。
    pub fn emit(&mut self, sess: &Session, kind: &str, extra: Value) -> Result<u64> {
        let seq = sess.next_seq();
        self.emit_seq(seq, sess, kind, extra)?;
        Ok(seq)
    }

    /// 用调用方指定的 seq 写事件（请求事件需要 seq 先定，好让点击关联指得准）
    pub fn emit_seq(&mut self, seq: u64, sess: &Session, kind: &str, extra: Value) -> Result<()> {
        // 注意顺序：先放 extra，再写公共字段。反过来的话 extra 里的 "seq"
        // 会把全局 seq 覆盖掉（弹窗序号就干过这事，导致 seq 重复）。
        let mut obj = Map::new();
        if let Value::Object(m) = extra {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }
        obj.insert("seq".into(), json!(seq));
        obj.insert("ts".into(), json!(now_iso()));
        obj.insert("ts_mono".into(), json!(sess.mono_ms()));
        obj.insert("kind".into(), json!(kind));
        obj.insert(
            "step".into(),
            match sess.step() {
                Some(s) => json!(s.as_str()),
                None => Value::Null,
            },
        );
        let line = Value::Object(obj).to_string();

        if let Some(f) = self.file.as_mut() {
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
            f.flush()?;
            // sync_data 在部分网络盘/杀软下会慢，但这是刻意的取舍
            if let Err(e) = f.sync_data() {
                tracing::warn!("timeline sync_data 失败（继续写）: {e}");
            }
        }

        if let Ok(mut q) = LAST_LINES.lock() {
            q.push_back(line);
            while q.len() > 50 {
                q.pop_front();
            }
        }
        Ok(())
    }
}
