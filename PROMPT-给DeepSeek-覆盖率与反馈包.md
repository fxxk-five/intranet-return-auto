# 任务：给已交付的 Rust 取证探针增加「覆盖率契约 + 反馈包 + 业务台账」

> 这是给 AI 编码助手的施工 prompt。**你不是在设计新方案，你是在给一个 6072 行的现有工程加四个模块。**
> 下面出现的所有结构体名、函数名、字段名、退出码都是从现有源码里读出来的真实值。
> 与你读到的不一致时，**以源码为准**，并在交付说明里记一条偏差——不要为了对这份 prompt 而改源码。

---

## 0. 背景与你必须理解的一件事

这个探针跑在**纯内网机**上，没有外网、没有包管理器、不能用 Python/Node。

现实约束是：**反馈回路极长**。开发机上一分钟一次编译，内网机上一次往返可能是几天，还要协调操作员时间。

所以这次要加的东西，本质是解决一个问题：

> **不能让操作员"跑完了、包带回来了、打开一看少一段"。**

探针目前的形态是**被动记录**：记录发生的一切，不知道"该发生的有没有发生"。它会在阶段 C 什么都没采到时，依然安静地输出一份十二节齐全的报告，C 节写"未获取到"。

这次要让它变成**主动回执**：自己知道缺什么，**缺就不让走**。

---

## 1. 现状（已核实，不要重新调研）

### 1.1 工程结构

```
probe/
├── Cargo.toml            # 已含 sha2、regex、once_cell、serde_json；刻意不含 reqwest
├── src/
│   ├── main.rs           # CLI 分发；退出码 0/1/2/3
│   ├── cli.rs            # Cmd: Preflight|Launch|Attach|Experiment|Report|Mask；Step: A|B|C|D
│   ├── lib.rs            # 27 个 pub mod
│   ├── app.rs            # attach 主循环，687 行，单任务 + 250ms tick
│   ├── session.rs        # Session::open / latest_dir / all_dirs / path / mark_finished / acquire_lock
│   ├── recorder.rs       # Recorder::emit(sess, kind, extra) -> seq；每行 sync_data
│   ├── netcap.rs         # NetStore::ingest_timeline(&Value)、scan_stats()、to_har()
│   ├── dialogs.rs        # DialogStore::ingest_timeline(&Value)、to_json()
│   ├── domino.rs / correlate.rs / experiment.rs / experiment_replay.rs
│   ├── report/           # mod.rs(528 行) + markdown.rs(304 行)
│   ├── hotkeys.rs        # 三级降级 + MOD_NOREPEAT + hotkeys.json
│   ├── redact.rs         # mask_number 等，11 个测试
│   ├── win32.rs / shot.rs / http1.rs / preflight.rs / mask.rs / launch.rs / util.rs
└── release/              # probe.exe(2.4MB) + 启动Edge.bat + operator-card.md
```

### 1.2 三个关键既有设计（你要复用的，不要另起一套）

**① 事件流是唯一事实源。**
`Recorder::emit` 给每条事件分配 `seq`、写 `timeline.jsonl`、`sync_data`。
`NetStore::ingest_timeline` 和 `DialogStore::ingest_timeline` 都能**从 timeline 行重建内存状态**，`report::rebuild(sess)` 正是靠这个，在崩溃后从纯 JSONL 重新生成全部产物。

**② 这意味着覆盖率也必须是纯函数。**
`coverage(timeline) -> Coverage`。实时显示和事后打包必须走同一条计算路径。
**不要**在 `App` 里维护一个递增计数器然后指望它跟 timeline 一致——断点续跑时它一定会错。

**③ `App::emit(&mut self, kind: &str, extra: Value) -> Result<u64>`** 是唯一的事件出口。

### 1.3 现有 31 个测试的分布

`correlate(2) dialogs(2) experiment(3) experiment_replay(2) hotkeys(3) http1(2) netcap(4) redact(11) util(2)`

**这 31 个必须继续全绿。** 改 `Session::open` 或 `Recorder` 时特别小心。

### 1.4 现有依赖（Cargo.toml 已含）

`tokio tokio-tungstenite futures-util serde serde_json clap tracing tracing-subscriber anyhow thiserror base64 image time crossbeam-channel regex once_cell sha2 windows(0.58)`

**唯一允许新增的依赖是 `zip`**（见 §7.1）。`sha2` 已存在，manifest 哈希和台账哈希直接用它。

---

## 2. 要交付的四个模块

| 模块 | 文件 | 职责 |
|---|---|---|
| 覆盖率契约 | `src/coverage.rs` | 从 timeline 算出「每一步该有的证据齐没齐」 |
| 业务台账 | `src/ledger.rs` | 单号 → 四个阶段的处理矩阵 |
| 反馈包 | `src/bundle.rs` | 一条命令产出不可否认、自校验的压缩包；不齐就拒绝 |
| 计划配置 | `src/plan.rs` + `plan.json` | 槽位规则与必选性，可改不重编译 |

加两个 CLI 子命令：`bundle`、`coverage`。

---

## 3. 覆盖率契约（`src/coverage.rs`）

### 3.1 数据模型

```rust
//! 覆盖率契约：从 timeline 事件流算出每个阶段的证据完整性。
//! 纯函数式：所有输入都来自 timeline.jsonl，与 NetStore/DialogStore 的重放路径一致。

/// 判定规则。全部在 plan.json 里可配，这里是默认值的内置副本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Rule {
    /// 存在一条 kind=="click" 且 element.text 包含 needle 的事件
    ClickText { needle: String },
    /// 存在一条 kind=="request"，method 匹配，url 不命中排除表
    Request { method: Option<String> },
    /// 某个 kind 的事件数 >= n
    KindCount { kind: String, min: usize },
    /// assertions.json 里 id 为 X 的断言 pass == true
    AssertionPass { id: String },
    /// elements.json 的 control_map 里至少 n 个 selector 唯一
    SelectorUnique { min: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotDef {
    pub id: String,          // 全局唯一，如 "B.phone_input"
    pub step: String,        // "A"|"B"|"C"|"D"|"G"（G=全局）
    pub title: String,       // 中文，直接给操作员看
    pub required: bool,      // 必选槽位不满足 → bundle 拒绝
    pub rule: Rule,
    pub hint: String,        // 缺失时告诉操作员"回去做什么"
}

#[derive(Debug, Clone, Serialize)]
pub struct SlotResult {
    pub id: String,
    pub step: String,
    pub title: String,
    pub required: bool,
    pub satisfied: bool,
    pub hits: usize,              // 命中次数
    pub sample_seqs: Vec<u64>,    // 最多 5 条证据的 seq
}

#[derive(Debug, Clone, Serialize)]
pub struct StepCoverage {
    pub step: String,
    pub cn: String,
    pub required_total: usize,
    pub required_ok: usize,
    pub optional_ok: usize,
    pub status: &'static str,     // "complete" | "partial" | "empty"
}

#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub rule_version: String,     // 默认为 crate version + 规则表哈希前 8 位
    pub generated_at: String,
    pub session_id: String,
    pub overall: Overall,
    pub steps: Vec<StepCoverage>,
    pub slots: Vec<SlotResult>,
    pub missing: Vec<Missing>,    // 只含 required && !satisfied
    pub warnings: Vec<String>,    // 见 §3.4
    pub mail_numbers: Vec<MailNoStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overall {
    pub required_total: usize,
    pub required_ok: usize,
    pub optional_total: usize,
    pub optional_ok: usize,
    pub ready: bool,              // required_ok == required_total
}
```

### 3.2 实现要求

```rust
impl Coverage {
    pub fn new(session_id: &str, plan: &Plan) -> Coverage;
    /// 喂一条 timeline 行（与 NetStore/DialogStore 同构）
    pub fn ingest_timeline(&mut self, v: &Value);
    /// 从磁盘完整重算。用于 coverage / bundle / report 命令。
    pub fn from_session(sess: &Session, plan: &Plan) -> Result<Coverage>;
    pub fn to_json(&self) -> Value;
    /// 控制台单行摘要，如 "A=完整 B=4/5 C=空 D=完整"
    pub fn status_line(&self) -> String;
    /// 缺什么、按什么顺序补，给操作员看的纯文本
    pub fn what_to_fix(&self) -> String;
}
```

**硬要求：**

1. `ingest_timeline` 必须能处理**任意 kind**，未知 kind 静默忽略，不 panic。
2. `from_session` 读 `timeline.jsonl` 逐行 `serde_json::from_str`，**坏行跳过并计数**（写在 `warnings` 里），不能整份失败。
3. `mail_numbers` 从 `kind=="mail_no"` 事件聚合（§4.2）。
4. 排序稳定：`slots` 按 `(step, id)` 排，`missing` 按必选优先 + step 顺序。**不依赖 HashMap 迭代顺序。**

### 3.3 默认槽位表（plan.json 首次生成时写入）

**G 全局**

| id | title | required | rule |
|---|---|---|---|
| `G.attach_ok` | 成功挂上浏览器 | ✅ | `KindCount{kind:"attach_started", min:1}` |
| `G.har_entries` | 至少捕获 1 条业务请求 | ✅ | `KindCount{kind:"request", min:1}` |
| `G.element_unique` | 至少 1 个控件有唯一定位器 | ✅ | `SelectorUnique{min:1}` |
| `G.target_attached` | 至少附着 1 个页面 target | ✅ | `KindCount{kind:"target_attached", min:1}` |

> ⚠️ **不要写 `KindCount{kind:"preflight"}`。** 已核实：`preflight` 命令**不写 timeline**，
> 它只把 `preflight.json` 写在 `<out>` 根目录（且 `app.rs` 里也没有任何 `emit("preflight")`）。
> 用这个 kind 做规则会导致槽位**永远变红**，造成假阴性。
> 环境自检结果在 `report.json` 的 `preflight` 段，与 timeline 是两条独立路径。
> 若确实需要在覆盖率里体现自检，改用 `Session::path("preflight.json").exists()` 判定，
> 但**默认不做**——自检失败时探针根本跑不起来，覆盖率没有机会计算。

> **已核实的 timeline kind 全集**（从这里选，不要发明）：
> `attach_started` `targets_list` `target_attached` `click` `change` `enter`
> `request` `response` `dialog_dom` `dialog_js` `native_window` `print_preview`
> `screenshot` `note` `error`
> 已核实 `enter` 由 `src/assets/record_click.js:86` 在 `keydown` 为 Enter 时发出，
> 并已在 `app.rs:469` 的 `"change" | "enter"` 分支里落到 timeline，**直接复用，不要重写**。

**A 下段扫描**

| id | title | required | rule / hint |
|---|---|---|---|
| `A.scan_click` | 点了"确定"输入单号 | ✅ | `ClickText{needle:"确定"}` / 回去再做一次单号输入 |
| `A.scan_request` | 捕获到下段扫描请求 | ✅ | `Request{method:Some("POST")}` / 检查是否走了缓存 |
| `A.confirm_scan` | 点了"确认下段" | ✅ | `ClickText{needle:"确认下段"}` / 阶段A 收尾要点这个按钮 |
| `A.mail_no` | 至少识别到 1 个邮件号 | ✅ | `KindCount{kind:"mail_no", min:1}` |
| `A.enter` | 有 Enter 键输入 | ❌ | `KindCount{kind:"enter", min:1}` |

**B 详情单补打**

| id | title | required | rule / hint |
|---|---|---|---|
| `B.print_click` | 点了"打印" | ✅ | `ClickText{needle:"打印"}` |
| `B.template` | 选了打印模板 | ❌ | `ClickText{needle:"面单"}` / 模板下拉里选"国内快包面单-正式" |
| `B.phone_input` | 出现收件人手机号弹窗 | ✅ | **`ClickText{needle:"手机号"}`** 或 `KindCount{kind:"dialog_dom",min:3}` / 这是 B 段唯一不可替代的证据 |
| `B.modal_burst` | 出现批量弹框（分支 B2） | ❌ | `KindCount{kind:"dialog_dom", min:2}` / 若能复现"弹一串框逐个点x"请务必做一次 |
| `B.rescan_ok` | 再次下段扫描弹出提示 | ❌ | `AssertionPass{id:"B_rescan_success"}` |

**C 退回寄件人 / 妥投确认**

| id | title | required | rule / hint |
|---|---|---|---|
| `C.return_click` | 点了"退回寄件人" | ✅ | `ClickText{needle:"退回寄件人"}` |
| `C.enter` | 逐单输入并按了 Enter | ✅ | `KindCount{kind:"enter", min:1}` / C 段每输一个单号都要按 Enter |
| `C.select_all` | 勾了全选 | ❌ | `ClickText{needle:"全选"}` |
| `C.deliver_confirm` | 点了"妥投确认" | ✅ | `ClickText{needle:"妥投确认"}` |
| `C.dialog_close` | 关闭了确认提示 | ❌ | `ClickText{needle:"关闭"}` |

**D 投递归班交接**

| id | title | required | rule / hint |
|---|---|---|---|
| `D.open` | 打开了投递归班交接 | ✅ | `ClickText{needle:"投递归班"}` |
| `D.query` | 点了"查询" | ✅ | `ClickText{needle:"查询"}` |
| `D.arrow` | 点了 "->" 转移按钮 | ✅ | `ClickText{needle:"->"}` / 该按钮可能无文本，见下方兜底 |
| `D.confirm` | 点了"确定" | ❌ | `ClickText{needle:"确定"}` |

> **`D.arrow` 的兜底**：`->` 按钮很可能是个图标按钮，`element.text` 为空。
> `ClickText` 规则的实现里，除 `text` 外还要匹配 `element.attrs.value`、`title`、`alt`、`aria_label`，
> 并且允许 needle 用正则（`^[-–—>→]+$`）。匹配不到时该槽位保持未满足，**不要为了让它变绿而放宽到"任意点击"**。

### 3.4 警告（`warnings`，不影响 ready，但必须显示）

1. **疑似漏切阶段**：全部 `click` 事件集中在某一个 step，且该 step 的槽位命中数远超其他 → 输出
   `"阶段 B/C/D 事件极少（A=37, B=0, C=0, D=0）。可能操作员忘了按热键切换阶段，或状态行切换失败。"`
   判定：`max_step_events >= 5 * 所有其他 step 事件和` 且 至少 2 个 step 为 0。
2. **请求全被排除表滤掉**：`request` 事件 > 0 但 `scan_candidates()` 为空 → 提示排除表可能过严。
3. **timeline 有坏行**：`"timeline.jsonl 有 N 行解析失败，已跳过。"`
4. **断点续跑**：`sess.resumed == true` → `"本次为续跑会话，时间跨度可能不连续。"`

### 3.5 单元测试（至少 6 个）

在 `coverage.rs` 底部 `#[cfg(test)]`，用构造的 `Value` 序列喂 `ingest_timeline`：

1. 空 timeline → `ready == false`，`missing` 含全部 required
2. 单条 `click{text:"确定"}` → `A.scan_click` 满足，其他不满足
3. 完整 A/B/C/D 最小事件集 → `ready == true`
4. 未知 kind → 不 panic，不改变结果
5. `mail_no` 聚合正确（3 条事件 2 个不同单号 → `mail_numbers.len() == 2`）
6. `status_line()` 输出格式符合 `A=完整 B=4/5 C=空 D=完整`

---

## 4. 业务台账（`src/ledger.rs`）

### 4.1 为什么需要

报告能告诉你"阶段 B 有 4 个弹窗"，但回答不了**"这 5 个测试单，有几个走完了全链路"**。
而这正是内网外复核时第一个要问的问题。

### 4.2 单号提取（在 `App` 里做，事件驱动）

**新增一种 timeline 事件 `mail_no`：**

```json
{"kind":"mail_no","raw_hash":"a3f5c1d2","masked":"7100*********161",
 "source":"request_body","request_id":"1234.5","key":"mailNo"}
```

**要求：**

1. **绝不落明文**。字段只有 `raw_hash`（`sha256(raw)` 前 8 位十六进制）和 `masked`（复用 `redact.rs` 现有函数）。
2. **提取时机**：`App` 处理完 `request` 事件后，用 `plan.json` 的 `mail_no_keys`（默认 `["mailNo","mail_no","mailno","numbers","waybillNo"]`）在 JSON body 里找值为 11–16 位纯数字的字段；非 JSON 才回退到正则。
3. **排除表照用**：已存在的 URL 排除逻辑（print/phone/deliver/return/handover/query/confirm）必须复用，**不要重新写一份**。
4. **去重**：同一 `raw_hash` 在同一 step 内只发一次 `mail_no` 事件。
5. **HTTP 时间戳防空**：`{"ts":1794897871136}` 的坑已经在 `count_numbers_in_body` 里踩过，`mail_no` 提取必须只认配置的键名——**不要**再加"任意 13 位数字"的兜底。（这是 README §9 偏差 #3，不许回退。）

### 4.3 `ledger.csv`

```csv
单号,单号标识,阶段A_下段扫描,阶段B_补打,阶段B_手机号弹窗次数,阶段C_妥投确认,阶段D_归班,状态
7100*********161,"sha8:a3f5c1d2",2026-11-17 14:02:11,,,,,未完成
7100*********204,"sha8:9b2e7a01",2026-11-17 14:02:11,2026-11-17 14:03:47,3,,,未完成
```

**编码：UTF-8 with BOM**（Excel 直接双击打开不乱码）。其余所有 JSON/HAR/MD 一律 **UTF-8 无 BOM**。
时间格式：`YYYY-MM-DD HH:MM:SS` 本地时间。

**关联规则（必须在 `00-先读我.txt` 里明说）：**

> 台账的关联是**请求级**的：某单号的 `mail_no` 事件落在哪个 step 的时间窗内，就记到哪个阶段。
> 它证明"这个单号的请求在阶段 B 期间发出过"，**不证明"业务已确认完成"**。
> 最终是否处理完，仍需在系统里核对。

### 4.4 实现要求

```rust
pub struct LedgerRow {
    pub masked: String,
    pub hash8: String,
    pub step_ts: [Option<String>; 4],   // A/B/C/D 首次出现时间
    pub phone_dialogs: Option<usize>,   // B 段该 step 内的 dom_modal 计数
    pub status: &'static str,           // "完成"|"未完成"
}

pub fn build(sess: &Session, cov: &Coverage, plan: &Plan) -> Result<Vec<LedgerRow>>;
pub fn write_csv(sess: &Session, rows: &[LedgerRow]) -> Result<PathBuf>;
```

判定"完成"：四个 `step_ts` 全为 `Some`。

---

## 5. 计划配置（`src/plan.rs` + `plan.json`）

### 5.1 为什么用 JSON 不用 TOML

**零新增依赖。** 工程里已有 `serde_json`；引入 `toml` 会多一个 vendored crate，而 vendor 目录已经 266 MB。
`plan.json` 由 `probe bundle --init-plan` 生成带注释字段的默认值，人改也不会太难。

### 5.2 schema

```json
{
  "plan_version": 1,
  "mail_no_keys": ["mailNo", "mail_no", "mailno", "waybillNo", "numbers"],
  "url_exclude": ["print", "phone", "deliver", "return", "handover", "query", "confirm"],
  "slots": [ /* SlotDef 数组，见 §3.1 */ ],
  "bundle": {
    "include_screenshots": true,
    "include_timeline": true,
    "max_bundle_mb": 200,
    "require_complete": true
  }
}
```

**加载规则：**
1. `--plan <path>` 指定；缺省读 `<out>/plan.json`；不存在则用**内置默认**（与 §3.3 表格一致）并写一份到 `<out>/plan.json`。
2. 解析失败 → 报错退出码 1，**不静默回退到默认**（否则人改了配置却没生效，会得出错误结论）。
3. `plan_version` 不认识的默认值 → 拒绝，提示升级探针。
4. `rule_version` 写进 `coverage.json` 和 `manifest.json`，值 = `format!("{}-{:08x}", crate_version, hash(plan_json))`。

---

## 6. 实时覆盖显示（`app.rs` 改动）

### 6.1 要求

操作员不会读 JSON。她需要在自己正在看的那个黑色窗口里，**当场知道缺什么**。

在 `App` 主循环里：

1. `App` 增加字段 `cov: Coverage`。
2. 每处理完一条事件后调 `self.cov.ingest_timeline(&v)`（`v` 就是传给 `emit` 的那个 `Value`，**不要复制一份再解析**）。
3. **输出时机**（避免刷屏）：
   - `sess.set_step()` 生效后立即打一行
   - 每 30 秒打一行（`tick % 120 == 0`，现有 tick 是 250ms）
   - 收到 stdin `STATUS` / `?` 时打完整版
4. **单行格式固定**：

```
[覆盖 14:07:22] A=完整 B=4/5 C=空 D=完整  → 必选 8/12（按 ? 看缺什么）
```

5. **完整版**（`?` 或 `help`）：

```
[覆盖] 必选 8/12
  A 下段扫描   完整 ✔
  B 详情单补打 4/5 ✘ 缺：B.phone_input —— 出现收件人手机号弹窗
  C 妥投确认   空   ✘ 缺：C.return_click、C.enter、C.deliver_confirm
  D 归班交接   完整 ✔
  警告：阶段 C 事件极少（A=37, C=0）。可能忘了按热键切换。
```

6. **stdin 命令扩充**（`spawn_stdin_control` 现有 `A/B/C/D/END`）：
   - `?` / `STATUS` → 打完整版
   - `BUNDLE` → 提示"请另开窗口运行 probe.exe bundle"，**不要在 attach 运行中打包**（会锁目录）
   - 其他未知输入 → 提示可用指令

### 6.2 硬要求

- **覆盖率计算不得阻塞事件循环。** 它是内存操作（槽位判定扫事件流），单条事件 O(slots)；若某条规则退化成 O(events)，就必须改为在 `ingest` 时增量累计。
- 单行输出用 `println!`，**不要用 `tracing`**——操作员只看得懂窗口里的行。
- 中文宽度对齐用 `util.rs` 现有工具，别自己数字节。

---

## 7. 反馈包（`src/bundle.rs`）

### 7.1 新增依赖（唯一一个）

```toml
zip = { version = "2", default-features = false, features = ["deflate"] }
```

**理由必须写进交付说明**：`deflate` 走 `flate2`/`miniz_oxide`，纯 Rust，不碰 C 工具链，符合工程禁令。
**禁止**启用 `bzip2`/`zstd`/`deflate-zlib`（会拉 C 库）。

### 7.2 命令

```
probe.exe bundle --out <DIR> [--dest <ZIP>] [--plan <PATH>]
                 [--force] [--with-secrets] [--no-screenshots] [--init-plan]
```

- `--dest` 缺省：`<out>/../../<hostname>-probe-<YYYYMMDD-HHMM>.zip`？**不要猜**。缺省写到 `%USERPROFILE%\Desktop\probe-反馈包-<YYYYMMDD-HHMM>.zip`；桌面不存在则写到 `<out>/bundle/`。
- `--init-plan`：只生成 `plan.json` 然后退出 0。
- `--force`：必选槽位不全时仍然打包，但 `manifest.json` 记 `"incomplete": true`，`00-先读我.txt` 第一段用大写写"**此包不完整，缺失项见下**"。
- `--with-secrets`：把 `secrets/` 打进包，**并且**在 `00-先读我.txt` 里加一行醒目警告。默认**不打**。
- `--no-screenshots`：跳过 `screenshots/`，`manifest.json` 记 `"skipped": ["screenshots"]`。

### 7.3 退出码（扩充 `main.rs` 的约定）

| 码 | 含义 |
|---|---|
| 0 | 打包成功且完整 |
| 1 | 一般错误（IO、磁盘满） |
| 2 | 参数非法（如 `--dest` 指向不可写路径） |
| 3 | 输出目录被占用（`Session` 锁） |
| **4** | **覆盖率不达标，未生成包**（新增） |

退出码 4 时的输出必须是**可执行的指令**，不是错误日志：

```
[FAIL] 反馈包未生成：必选证据缺 4 项。
       阶段 C（退回寄件人/妥投确认）完全未捕获。
       请补做：
         1. C 段：点"退回寄件人"，逐个输入单号并按 Enter，勾全选，点"妥投确认"
         2. B 段：让系统弹出收件人手机号输入框（必须有一个真实样本）
       补做后重新运行本命令。
       如确实无法补做，加 --force 生成不完整包（会标注 incomplete）。
```

### 7.4 包结构

```
probe-反馈包-20261117-1530/
├── 00-先读我.txt          ← 纯文本，UTF-8 with BOM，CRLF
├── manifest.json          ← 自校验索引
├── report.md
├── report.json
├── coverage.json
├── ledger.csv             ← UTF-8 with BOM
├── network.har
├── elements.json
├── dialogs.json
├── experiment21.json
├── assertions.json
├── timeline.jsonl         ← 可选（plan.bundle.include_timeline）
├── plan.json              ← 实际生效的配置副本
├── snapshots/
├── screenshots/           ← 可选
├── replay/{scenario.json,README.md}
└── secrets/               ← 仅 --with-secrets
```

**包内文件名一律用中文的地方保持中文**（`00-先读我.txt`），写 zip 时必须设置 **UTF-8 文件名标志位（bit 11）**，否则 Win11 资源管理器解压出乱码。这是必须测的：解压后文件名正确。

### 7.5 `manifest.json`

```json
{
  "probe_version": "0.1.0",
  "rule_version": "0.1.0-a3f5c1d2",
  "created_at": "2026-11-17T15:30:12+08:00",
  "session_id": "20261117-143022",
  "incomplete": false,
  "missing": [],
  "skipped": [],
  "bundle_sha256": "<zip 自身哈希>",   // 注意：写在包内，值为整包哈希，仅作留存比对
  "files": [
    {"path": "report.md", "size": 8123, "sha256": "..."},
    {"path": "screenshots/page-A-001.png", "size": 40211, "sha256": "..."}
  ]
}
```

**要求：**
- `files` 按 `path` 字典序排序，**稳定**。
- 每个文件写包前现算 `sha256`（用已有 `sha2`）。
- 排除 `manifest.json` 自身（无法自哈希）。
- 提供 `probe.exe bundle --verify <zip>`：解包到临时目录，逐文件比对哈希，输出 `OK` / 列出不匹配项。**这是"包没被改过"的唯一证据。**

### 7.6 `00-先读我.txt` 内容（固定骨架，逐段必须有）

```
【这份包是什么】
本次内网取证探针的运行结果。共采集 N 条事件，覆盖 4 个业务阶段中的 X 个。

【完不完整】
必选证据 12 项中满足 12 项。✅ 完整
（不完整时：❌ 缺 N 项，详见下）

【缺什么】
（无缺失则写"无"）
- B.phone_input：出现收件人手机号弹窗
  补做方法：让系统弹出手机号输入框，必须留一个真实样本

【怎么读】
先看 report.md 第一节（一句话结论）。Q1–Q7 逐条在第二节往后。

【包内文件】
report.md        人读结论
coverage.json    证据完整性明细（机器可读）
ledger.csv       单号→阶段处理台账（Excel 可直接打开）
network.har      全部请求，可用 Edge DevTools 导入
timeline.jsonl   原始事件流

【保密】
本包可能含业务数据与账号凭据。仅限内网留存。
（若含 secrets/：本包含明文凭据，禁止外传，用后删除。）
```

### 7.7 与 `Session` 的交互（**最容易出错的地方**）

`bundle` 需要读一个**已存在**的 session 目录，而 `Session::open` 的语义是"创建或续用未完成的 session"。
直接调用会在没有 session 时**凭空创建一个空目录**，然后生成一份"什么都缺"的包——正是要避免的假象。

**必须做的重构：**

```rust
impl Session {
    /// 现有：创建（或续用未完成）。语义不变，31 个测试依赖它。
    pub fn open(root: &Path) -> Result<Arc<Session>>;

    /// 新增：只读打开已存在的 session，绝不创建目录。
    /// - 取锁失败 → 复用现有退出码 3 的路径
    /// - 无 session 目录 → 明确报错（退出码 1），信息："输出目录里没有任何采集会话，请先运行 attach。"
    /// - finished 标记存在 → 照常打开（打包已完成会话是正常场景）
    pub fn open_existing(root: &Path) -> Result<Arc<Session>>;
}
```

`open_existing` 内部复用 `Session::latest_dir`，但**不要调用 `fs::create_dir_all`**，也不写 `out/latest`。

`probe coverage --out <DIR>` 用同一个 `open_existing`。

### 7.8 大小与安全

- 打包前累计预计大小，超 `plan.bundle.max_bundle_mb`（默认 200）时**警告并继续**，把提示写进 `00-先读我.txt`；超 500 MB 直接拒绝（退出码 1），提示加 `--no-screenshots`。
- 包写完后，若目标已存在同名文件 → **默认拒绝覆盖**（退出码 2），`--force-overwrite` 才覆盖。反馈包是不可再生的一次性证据，覆盖是事故。

### 7.9 单元测试（至少 4 个）

1. manifest 哈希可复算：构造两个小文件 → 打包 → `--verify` 通过
2. 篡改包内文件 → `--verify` 失败并列出该文件
3. `incomplete: true` 时 `00-先读我.txt` 含"❌"与缺失项标题
4. 中文文件名（`00-先读我.txt`）在 zip 中带 UTF-8 标志位

---

## 8. CLI 变更（`src/cli.rs` + `src/main.rs`）

```rust
pub enum Cmd {
    Preflight(..), Launch(..), Attach(..), Experiment(..), Report(..), Mask(..),
    /// 计算并打印覆盖率（只读，不打包）
    Coverage(CoverageArgs),
    /// 生成反馈包；不完整则拒绝
    Bundle(BundleArgs),
}

#[derive(Args, Debug)]
pub struct CoverageArgs {
    #[arg(long)] pub out: PathBuf,
    #[arg(long)] pub plan: Option<PathBuf>,
    #[arg(long)] pub json: bool,        // 输出 coverage.json 到 stdout
}

#[derive(Args, Debug)]
pub struct BundleArgs {
    #[arg(long)] pub out: PathBuf,
    #[arg(long)] pub dest: Option<PathBuf>,
    #[arg(long)] pub plan: Option<PathBuf>,
    #[arg(long)] pub force: bool,
    #[arg(long)] pub with_secrets: bool,
    #[arg(long = "no-screenshots")] pub no_screenshots: bool,
    #[arg(long = "init-plan")] pub init_plan: bool,
    #[arg(long = "force-overwrite")] pub force_overwrite: bool,
    /// 校验已有包的哈希：`--verify <zip>`
    #[arg(long, value_name = "ZIP")] pub verify: Option<PathBuf>,
}
```

**`main.rs` 有两处要同步改：**
1. `panic_out` 的 `match` 要覆盖新命令（`Coverage(b) => b.out.clone()`、`Bundle(b) => b.out.clone()`）
2. `dispatch` 的 `match` 加两个分支

漏任何一处都会编译失败，这是好事——别用 `_ =>` 图省事，退出码信息会丢。

---

## 9. 集成点（精确）

| 位置 | 改动 |
|---|---|
| `src/lib.rs` | `pub mod coverage; pub mod ledger; pub mod plan; pub mod bundle;` |
| `Cargo.toml` | 加 `zip`；其余不动 |
| `src/app.rs` `struct App` | 加字段 `cov: Coverage`、`plan: Plan` |
| `src/app.rs` `App::emit` | 不改签名；在调用处把**同一个 `Value`** 喂给 `self.cov.ingest_timeline(&v)` |
| `src/app.rs` `spawn_stdin_control` | 加 `?`/`STATUS`/`BUNDLE` 分支 |
| `src/app.rs` 主循环 | 按 §6.1 时机打印状态行；收尾时写 `coverage.json`、`ledger.csv` |
| `src/report/mod.rs` `Input` | 加 `pub coverage: Option<&'a Value>`；`write_all` 里写 `coverage.json` |
| `src/report/markdown.rs` | 报告新增一节「证据完整性」，位置在「一句话结论」之后、Q1 之前；内容渲染 `status_line` + 缺失表 |
| `src/cli.rs` | 两个新 Subcommand + 两个 Args |
| `src/main.rs` | `panic_out` 与 `dispatch` 各加两支 |
| `src/session.rs` | 加 `open_existing`；`open` 语义不变 |
| `operator-card.md` | 加一步：结束前按 `?` 看覆盖，全绿才 END；再加一步：双击 `打包反馈包.bat` |

**报告骨架变更**：十二节变十三节，`report.md` 的标题层级和顺序其余部分**不动**（有一份验收清单依赖它）。

---

## 10. 禁令（违反即返工）

1. ❌ 不要为了"让覆盖率变绿"而放宽槽位判定。**绿的假比红更危险。**
2. ❌ 不要在任何产物里写明文单号或手机号。台账用 `masked` + `hash8`。
3. ❌ 不要引入除 `zip` 之外的依赖。
4. ❌ 不要在 `attach` 运行中打包（锁冲突）。
5. ❌ 不要 `Session::open` 来读已存在的 session（会造空目录）。
6. ❌ 不要改 `Recorder::emit` 的签名或写入语义（`sync_data` 是刻意取舍）。
7. ❌ 不要让覆盖率计算阻塞事件循环。
8. ❌ 不要回退 README §9 的任何一条偏差（尤其 #3 单号计数、#5 打印预览判定、#7 网络打印机超时）。
9. ❌ 不要覆盖已存在的反馈包（除非显式 `--force-overwrite`）。
10. ❌ 不要在 `report.md` 里编造数据。没采到就写"未获取到（原因：…）"。

---

## 11. 验收（必须全过才算交付）

### 11.1 回归

- [ ] `cargo test` **≥ 41 passed / 0 failed**（31 原有 + ≥10 新增）
- [ ] `cargo build --release --offline` 成功，产物仍为单 exe
- [ ] 原有 12/12 H2 端到端、12/12 H1、10/10 续跑 **全部仍通过**

### 11.2 新功能

- [ ] mock 页跑完 A→B→C→D 完整流程后 `probe coverage` 输出 `必选 12/12`
- [ ] 只跑 A→B 时 `probe coverage` 明确列出 C/D 缺失项
- [ ] `probe bundle` 在只跑 A→B 时**退出码 4**，且输出里含具体的"请补做"步骤
- [ ] `probe bundle --force` 生成包，`00-先读我.txt` 含"❌"和缺失明细
- [ ] 包解压后 `00-先读我.txt`、`ledger.csv` 文件名正常，Excel 打开 `ledger.csv` 不乱码
- [ ] `probe bundle --verify <zip>` 对未篡改包输出 OK，篡改后能定位到具体文件
- [ ] `probe bundle --init-plan` 生成的 `plan.json` 与 §3.3 默认槽位表一致
- [ ] 改 `plan.json` 把某槽位设 `required:false` 后，同一份数据 `bundle` 通过（证明配置真的生效）
- [ ] `probe coverage --out <空目录>` 退出码 1，**不创建任何目录**
- [ ] `probe report` 在空目录仍退出码 0、不 panic（原有行为不回退）
- [ ] 断点续跑后覆盖率**不重复计数**（同一事件只喂一次）

### 11.3 证据样本

交付时附一份真实 `coverage.json` + `ledger.csv` + `00-先读我.txt`（mock 产生的即可），**不要只写"已验证"**。

---

## 12. 实施顺序（每步独立可验收，不要跳）

| # | 内容 | 验收 |
|---|---|---|
| 1 | `plan.rs` + `plan.json` 默认表 + `--init-plan` | 生成的文件与 §3.3 一致 |
| 2 | `coverage.rs` 模型 + `ingest_timeline` + 6 个单测 | `cargo test` 绿 |
| 3 | `Session::open_existing` + 单测 | 空目录不建目录 |
| 4 | `Cmd::Coverage` + `coverage --json` | 对 mock 会话输出正确 |
| 5 | `mail_no` 事件提取（`app.rs`）+ `ledger.rs` + CSV | 台账行数 = 单号数 |
| 6 | `app.rs` 实时状态行 + stdin 命令 | mock 上肉眼可见、不刷屏 |
| 7 | `report` 集成（新一节 + `coverage.json`） | 十三节齐全 |
| 8 | `bundle.rs` 打包 + manifest + UTF-8 文件名 | 解压文件名正常 |
| 9 | `bundle --verify` + 4 个单测 | 篡改能定位 |
| 10 | `--force` 路径 + `00-先读我.txt` 渲染 | 不完整包标注清晰 |
| 11 | `operator-card.md` + `打包反馈包.bat` | 操作员能照着做 |
| 12 | 重新 `cargo vendor` + 重打 `release/` | 在干净 Win11 上 `--offline` 构建通过 |

---

## 13. 交付物

1. 源码改动（估计 +1300 行）
2. 更新后的 `release/` 三件套 + 新增 `打包反馈包.bat`
3. `README.md` 新增一节「覆盖率与反馈包」，**并更新 §9 偏差表**（这次至少会新增 2 条：`plan.json` 用 JSON 不用 TOML；`Session::open_existing` 的必要性）
4. 一份 mock 产出的真实样本包
5. 一段说明：**这次改动后，操作员在内网机上的完整动作序列是什么**（一句话一步，共 ≤8 步）

---

## 14. 最后一条

这次改动的成败**不看代码质量，看一件事**：

> 操作员在内网机上跑完，**能不能在她离开那台机器之前，就知道这次采集够不够**。

如果她还是要"打包带回来等人看"，那这次就白做了。
