# 内网退货自动化 · 取证探针（Rust）实施规格书 v1

> 本文档是给实施方（AI 编码助手）的**施工图**，不是讨论稿。
> 所有函数签名、文件名、CLI 参数、JSON 字段名、错误信息均为规定，不得自行改名。
> 未在本文档中明确允许的行为，一律不实现。

---

## 0. 文档定位

本探针**不是**退货自动化程序的第一个版本，**是**一次性取证工具。

它运行在一个内网 Windows 11 机器上，操作员手工走一遍退货流程（见 §1），探针在后台旁路记录全部证据，最终产出一份能回答 §2 全部问题的报告。

**探针不做自动化点击。** 唯一例外是 §7.7 的受控实验，且必须显式开启。

---

## 1. 业务背景（已知事实，不要再调研）

### 1.1 系统环境（已确认，不要再提问）

| 项 | 值 |
|---|---|
| 浏览器 | Microsoft Edge，**Chromium 内核**（已确认，不用考虑 IE 兼容模式） |
| 操作系统 | Windows 11 |
| 网络 | 纯内网，**无外网**，不能 `pip install` / `npm install` / `cargo add` |
| 运行方式 | 单台内网机，单实例串行操作 |
| 物理打印 | **不发生**。打印链路不接真实打印机（虚拟打印机或直接取消） |
| 日单量 | 200–500 单/天 |
| 收件人手机号 | 固定值，所有单统一填同一个（示例 `13000000000`），不需要查询 |

### 1.2 目标系统

内网寄递业务平台，新一代内网 Web 系统。需同时打开 **4 个业务标签页**：

1. 下段扫描
2. 详情单补打
3. 退回寄件人（含妥投确认）
4. 投递归班交接

### 1.3 手工流程（据内部 Word SOP 整理）

```
[输入] Excel，单列，只有邮件号，行数不固定（200-500）
  │
  ├─ 阶段A 下段扫描
  │    在"邮件号码"输入框逐个输入单号 → 每个单号点一次"确定"
  │    经验值：一次连续做 21 个（21 的来源不明，见 §2 Q2）
  │    全部输完后点"确认下段"
  │
  ├─ 阶段B 详情单补打
  │    把阶段A 用过的单号拼接成 "单号1,单号2,..."（英文逗号，无空格，无尾随逗号）
  │    粘贴进输入框 → 点"查询" → 勾"全选" → 打印模板选"国内快包面单-正式"
  │    → 点"打印" → 点"确定打印"
  │    然后出现两种分支：
  │      分支B1（个别单）：无额外弹窗，直接完成
  │      分支B2（批量）：弹出一串提示框，需要逐个点 x 关掉
  │    → 关完后 → 勾"全选" → 模板选"国内快包面单-正式" → 点"打印"
  │    → 弹出"填写收件人手机号"框 → 填固定手机号 → 点"确定"
  │    → 需要再点一次"打印"才会弹下一个 → 循环，直到不再弹框
  │    → 完成后还有一批框要点 x
  │    成功判据：再次点"下段扫描"，输入邮件号会弹出提示框
  │
  ├─ 阶段C 退回寄件人 / 妥投确认
  │    点"退回寄件人" → 逐个输入单号 → 每个输完按 Enter → 结果列表出现
  │    → 勾"全选" → 点"妥投确认" → 点弹出的"关闭"
  │
  └─ 阶段D 投递归班交接
       投递员下拉选固定值（示例"张三"）→ 点"查询" → 勾"全选"
       → 点"确定" → 点 "->" 按钮 → 结束
```

### 1.4 SOP 中已知的两个坑

- 单号拼接**不能有多余空格**，逗号必须是英文逗号
- 阶段B 的手机号弹框是**一次一个**，每填一个要点一次"确定"，且必须**再点一次"打印"**才会弹下一个

---

## 2. 探针必须回答的问题（唯一验收目标）

| ID | 问题 | 影响什么 |
|---|---|---|
| **Q1** | 四个业务步骤各自发出的 HTTP 请求是什么？明文 http 还是 https？有无签名/token/加密参数？ | 决定后续走"协议回放"（快 10 倍）还是"UI 点击" |
| **Q2** | 21 这个数字到底是什么？（见下方三个假设） | 决定批次编排复杂度，可能是最大的复杂度来源 |
| **Q3** | 各业务控件是否有稳定、唯一的定位器？ | 决定 UI 自动化能否长期维护 |
| **Q4** | 4 个"标签页"是 4 个 browser tab、4 个 iframe，还是 1 个页面里的 4 个区域？ | 决定上下文管理 |
| **Q5** | 阶段B 的两种分支（直接完成 / 弹一串框）由什么触发？ | 文档没写，必须实测 |
| **Q6** | 每单全链路耗时多少？500 单外推多久？ | 决定是否需要优化/并发 |
| **Q7** | 打印链路在无物理打印时到底弹什么窗口？能否绕过？ | 决定自动化里最脏的一段怎么处理 |

### Q2 的三个互斥假设（探针必须给出判定）

| 假设 | 内容 | 判定方法 |
|---|---|---|
| **H1** | 输入框有 `maxlength=21`，前端硬限制 | 读 DOM 属性 + 程序化写入 30 字符后读回 |
| **H2** | **根本没有 21 限制**，只是操作员"输一个点一次确定"，21 是她们心理上的批次大小 | 网络取证：看每次"确定"是一次单号请求还是批量请求 |
| **H3** | 后端有阈值，超过就报错 | 协议重放：N=21/22/30 逐一发，看响应 |

> **H2 的可能性很高，且是最重要的判定。** 如果每次"确定"只发 1 个单号的请求，那 200 单就是 200 次请求，跟 21 毫无关系，"批次"这个概念根本不存在，后续程序会简单一个数量级。
> 探针必须单独给出 `q2_verdict`，值为 `H1` / `H2` / `H3` / `unknown`。

---

## 3. 技术选型与理由

### 3.1 语言：Rust

理由：
- 产物是单个 exe，内网机零运行时依赖（无 Python / 无 Node / 无 JRE）
- 静态链接，不怕内网机缺 VC++ 运行库
- 便于后续把同一份代码扩展成正式程序（探针和正式程序共用模块）

### 3.2 关键决策：自己实现 CDP 客户端，**不要**用 `chromiumoxide`

**这是硬性规定。** 理由：

1. `chromiumoxide` 的 API 在小版本间变动频繁，且强绑定 `futures` 单线程模型，多 target 场景写法别扭。
2. 我们需要**完整的原始事件流**（`Network.*`、`Runtime.bindingCalled`、`Page.javascriptDialogOpening`），高层封装会隐藏细节。
3. CDP 协议本身极简单：`{id, method, params, sessionId}` 发命令，`{id, result|error}` 回结果，`{method, params, sessionId}` 推事件。手写约 400 行，可控性远高于封装配。

用 `tokio-tungstenite` 直接收发 WebSocket 文本帧。

### 3.3 依赖清单（Cargo.toml）

```toml
[package]
name    = "probe"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.24"
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "time"] }
anyhow = "1"
thiserror = "2"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
base64 = "0.22"
image = { version = "0.25", default-features = false, features = ["png"] }
time = { version = "0.3", features = ["formatting", "macros"] }
crossbeam-channel = "0.5"

[dependencies.windows]
version = "0.58"
features = [
  "Win32_Foundation",
  "Win32_UI_WindowsAndMessaging",
  "Win32_Graphics_Gdi",
  "Win32_System_Threading",
  "Win32_System_ProcessStatus",
  "Win32_System_Console",
  "Win32_Graphics_Printing",
  "Win32_Storage_Xps",
]

[profile.release]
opt-level    = "z"
lto          = true
codegen-units = 1
panic        = "abort"
strip        = true
```

**版本要求**：上面的版本号是下限参考，实施时在**有外网的开发机**上 `cargo update` 到最新兼容版，然后 `cargo vendor` 打包一段 `vendor/` 目录，连同 `Cargo.lock` 一起交付。内网机只做 `cargo build --release --offline`。

- 不要用 `native-tls` / `openssl`（内网机可能没有 openssl）
- 不要用 `screenshots` / `win-screenshots` 之类的第三方截图库，自己用 GDI 调，避免引入不透明行为
- 不要引入任何需要 C 编译器或 cmake 的 crate

### 3.4 不使用的能力（明确排除）

- ❌ Selenium / WebDriver / msedgedriver（多一层版本耦合，Edge 升级就断）
- ❌ UIA / Accessibility API（已确认 Chromium 内核，用不上）
- ❌ 图像识别 / OCR / 坐标点击
- ❌ 任何把凭据写入 exe 内部的做法

---

## 4. 交付物

### 4.1 代码交付

一个 Cargo 工程，可通过 `cargo build --release --offline` 编译出 `probe.exe`。

### 4.2 运行产物（探针跑完后在 `--out` 目录下）

```
<out>/
├── report.md                ← 人读的一页纸结论（格式见 §9.1）
├── report.json              ← 机读的同样内容
├── network.har              ← 标准 HAR 1.2，可导入 Edge DevTools
├── elements.json            ← 全部可交互元素清单 + 定位器（§8.2）
├── dialogs.json             ← 弹窗分类表（§8.3）
├── experiment21.json        ← Q2 判定结果（§8.4）
├── assertions.json          ← 流程里程碑断言（§8.5）
├── timeline.jsonl           ← 全事件流，逐行 JSON，可续跑（§8.1）
├── snapshots/               ← 每次 step 的 DOM 快照（脱敏后）
├── screenshots/             ← 页面截图 + 原生窗口截图
├── replay/
│   ├── scenario.json        ← 最小请求序列（去噪后的可重放模板）
│   └── README.md            ← 每个请求说明"对应 SOP 哪一步"
└── secrets/
    └── secrets.json         ← 完整凭据/单号（不脱敏），单独目录，报告首行警告
```

### 4.3 附带交付

- `README.md`：编译方法、内网部署方法、运行手册
- `mock/`：本地模拟页（§11），用于无内网环境下自测
- `operator-card.md`：操作员操作卡（§12）

---

## 5. 工程结构

```
probe/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── operator-card.md
├── vendor/                        ← cargo vendor 产物
├── mock/
│   ├── index.html                 ← 模拟业务页
│   ├── app.js
│   └── style.css
└── src/
    ├── main.rs                    ← 入口，CLI 分发
    ├── cli.rs                     ← clap 定义
    ├── session.rs                 ← session 目录、续跑、单实例锁
    ├── cdp/
    │   ├── mod.rs                 ← CdpClient
    │   ├── transport.rs           ← WebSocket 收发
    │   ├── commands.rs            ← 强类型命令封装
    │   └── events.rs              ← 事件分发
    ├── preflight.rs               ← 环境自检（§7.2）
    ├── recorder.rs                ← JSONL 写入器
    ├── netcap.rs                  ← 网络取证（§7.3）
    ├── domino.rs                  ← DOM 清单（§7.4）
    ├── correlate.rs               ← 点击↔请求关联（§7.5）
    ├── dialogs.rs                 ← 弹窗分类（§7.6）
    ├── experiment.rs              ← 21 阈值实验（§7.7）
    ├── shot.rs                    ← 截图（§7.8）
    ├── win32.rs                   ← 窗口枚举/截图/打印机（§7.8）
    ├── redact.rs                  ← 脱敏（§10）
    ├── report/
    │   ├── mod.rs
    │   ├── har.rs
    │   ├── elements.rs
    │   └── markdown.rs
    └── assets/
        ├── inspect_dom.js
        ├── record_click.js
        └── mutation_probe.js
```

`assets/*.js` 用 `include_str!` 内嵌，**不需要运行时读外部文件**（保证单 exe）。

---

## 6. CLI 规格

```
probe.exe <COMMAND> [OPTIONS]
```

### 6.1 `preflight` —— 环境自检

```
probe.exe preflight [--port 9222]
```
检查项见 §7.2，输出人读结果到 stdout + 写 `preflight.json`。

### 6.2 `launch` —— 探针拉起 Edge 并观测

```
probe.exe launch \
  --out <DIR> \
  [--edge <PATH>] \
  [--port 9222] \
  [--profile <DIR>]
```
行为：启动 Edge（带 §7.1 的参数）→ 等待调试端口就绪 → 转 `attach` 逻辑。

### 6.3 `attach` —— 挂到已运行的 Edge（**主用模式**）

```
probe.exe attach \
  --out <DIR> \
  [--port 9222] \
  [--step <A|B|C|D>] \
  [--hotkeys <SPEC>] \
  [--no-screenshot] \
  [--include-cookies]
```
`--step` 指定当前操作员正在走的阶段，用于给事件打标；运行中可用键盘热键切换（见 §7.5.4），热键表可用 `--hotkeys <SPEC>` 覆盖。

### 6.4 `experiment` —— Q2 阈值实验（会发真实请求）

```
probe.exe experiment \
  --out <DIR> \
  --port 9222 \
  --request-id <CAPTURED_REQUEST_ID> \
  --numbers 21,22,30 \
  --confirm-dry-run
```
- 不带 `--confirm-dry-run` 直接拒绝执行，退出码 2。
- 详细见 §7.7。

### 6.5 `report` —— 从已有事件流重新生成报告

```
probe.exe report --out <DIR>
```
用于崩溃后补救。

### 6.6 `mask` —— 单独对某个目录做脱敏

```
probe.exe mask --out <DIR>
```

---

## 7. 模块实现规格

### 7.1 Edge 启动参数（`launch` 模式）

```
edge_path = C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe
（不存在则依次尝试 C:\Program Files\Microsoft\Edge\Application\msedge.exe）
```

参数：

```
--remote-debugging-port=9222
--remote-allow-origins=*
--user-data-dir=<profile_dir>
--no-first-run
--no-default-browser-check
--disable-features=msEdgeSidebarV2
--start-maximized
about:blank
```

**关键必须点（会踩的坑，逐条实现）：**

1. **`--user-data-dir` 必须是独立目录**，不能指向用户默认 profile。
   原因：如果本机已有 Edge 在运行，新进程不会拿调试端口，而是直接移交给已存在的实例，9222 永远不监听。
   默认值：`%LOCALAPPDATA%\probe\profile`。
2. **`--remote-allow-origins=*` 必须加**。Chromium 111+ 起，非 DevTools 前端的 WebSocket 连接会被 403 拒绝。
3. 启动后**轮询** `GET http://127.0.0.1:<port>/json/version` 直到成功，最多 30 秒，每 250 ms 一次。超时则报错并打印 `tasklist | findstr msedge` 的输出，方便排障。
4. 用 `Command::new` 启动，**不要**用 `ShellExecute`；子进程句柄要保存，探针退出时不要强杀（操作员还要继续用浏览器）。
5. profile 目录里会留下登录态 cookie，`README` 要写明：**该目录等同于账号凭据，不得拷贝出内网机**。

### 7.2 `preflight` 自检项

逐项检查，全部写进 `preflight.json`，任一失败在 stdout 用 `[FAIL]` 标注：

| 检查 | 方法 | 失败后果 |
|---|---|---|
| Edge 可执行文件存在 | `Path::exists` 两个候选路径 | 阻断 |
| Edge 版本 | `GET /json/version` 的 `Browser` 字段，或读文件版本信息 | 仅记录 |
| 调试端口可连 | `GET /json/version` 超时 2s | 阻断 |
| **企业策略未禁用远程调试** | 读注册表 `HKLM\SOFTWARE\Policies\Microsoft\Edge\RemoteDebuggingAllowed`（`DWORD`，`0`=禁用）；同时读 `HKLM\SOFTWARE\Policies\Microsoft\Edge\DeveloperToolsAvailability`（`2`=禁用） | 阻断，提示联系 IT |
| 当前是否已有 Edge 实例 | `tasklist` / `sysinfo` 查 msedge.exe | 提示（有实例时 launch 必须用独立 profile） |
| 输出目录可写 | 创建临时文件并删除 | 阻断 |
| 是否管理员运行 | 非必需，仅记录 | 仅记录 |
| 默认打印机 | `GetDefaultPrinterW` | 仅记录（用于 Q7） |
| 打印机列表 | `EnumPrintersW` | 仅记录 |
| 系统 DPI | `GetDpiForSystem` | 仅记录（截图分辨率） |
| 磁盘剩余空间 | `GetDiskFreeSpaceExW` | < 2 GB 时警告 |

**注册表读取**：用 `windows` crate 的 `Win32_System_Registry` feature 补上（上面 features 列表里需要追加 `"Win32_System_Registry"`）。

### 7.3 网络取证（`netcap`）

#### 7.3.1 启用

对每个已 attach 的 page session 发：

```json
{"id":1,"method":"Network.enable","params":{
  "maxTotalBufferSize": 104857600,
  "maxResourceBufferSize": 10485760,
  "maxPostDataSize": 1048576
},"sessionId":"<sid>"}
```

同时 `Page.enable`、`Runtime.enable`、`DOM.enable`。

#### 7.3.2 订阅的事件

| 事件 | 处理 |
|---|---|
| `Network.requestWillBeSent` | 建记录，key = `requestId` |
| `Network.requestWillBeSentExtraInfo` | 补 headers（Chrome 会把 cookie 放这里） |
| `Network.responseReceived` | 补状态码、响应头、mimeType、timing |
| `Network.loadingFinished` | 标记完成，记录 `encodedDataLength` |
| `Network.loadingFailed` | 标记失败，记录 `errorText` |
| `Network.webSocketCreated` / `webSocketFrameReceived` | 单独记录（该平台可能用 WebSocket 推送） |

#### 7.3.3 采样规则（避免日志爆炸）

- **所有** `Document` / `XHR` / `Fetch` / `WebSocket` 类型：全量记录
- **所有** `http://` 请求：全量记录，并打 `plaintext: true`
- `Script` / `Stylesheet` / `Image` / `Font`：**只记 URL + 状态码**，不取 body
- 心跳/埋点：URL 匹配 `/(heartbeat|ping|track|log|monitor|report)[\/?]/` 的，只记 URL

#### 7.3.4 响应体

对 `XHR`/`Fetch`/`Document` 且状态码 < 400 的，在 `loadingFinished` 后调：

```json
{"id":N,"method":"Network.getResponseBody","params":{"requestId":"<rid>"},"sessionId":"<sid>"}
```

- 结果 `base64Encoded: true` 时用 base64 解码
- **失败要容忍**（资源已被回收时 CDP 会报 `Could not find object`），失败写 `body_status: "unavailable"`
- 响应体 > 2 MB 时只存前 2 MB + `truncated: true`
- 二进制体（`Content-Type` 非 text/json/xml/html）只记长度和 sha256，不存内容

#### 7.3.5 HAR 导出（`report/har.rs`）

按 HAR 1.2 规范手写，`writer`/`creator` 字段填 `probe 0.1.0`。
`entries[].startedDateTime` 用 ISO8601（UTC，毫秒精度）。
`entries[].time` 用毫秒浮点。
`entries[].request.postData.text` 存放**脱敏后**的 body（完整值在 `secrets/`）。
不存 `cookies` 字段内容（除非 `--include-cookies`）。

### 7.4 DOM 元素清单（`domino`）

#### 7.4.1 必须处理的情况

- 顶层 document
- 所有同源 `<iframe>`（递归，最大深度 5）
- 所有 `open` 状态的 Shadow DOM（递归遍历 `shadowRoot`，最大深度 5）
- 跨域 iframe：通过 `Target.setAutoAttach` 自动接管为独立 session 后再跑一次

#### 7.4.2 遍历范围

不要遍历所有 `*`（一个企业级页面可能上万节点，注入会卡）。
只采集：

```
input, textarea, select, button, a[href], [role=button], [role=checkbox],
[role=dialog], [onclick], [class*="btn"], [class*="button"],
label, iframe, table, form, textarea,
[class*="el-select"], [class*="el-dialog"], [class*="el-message"],
any element whose computed style position == fixed and z-index > 100
```

上限 3000 个元素，超出截断并记录 `elements_truncated: true`。

#### 7.4.3 每个元素输出（schema 见 §8.2）

关键字段说明：

- `visible`：`getBoundingClientRect()` 宽高 > 0，且 `getComputedStyle().display != none`，`visibility != hidden`，`opacity != 0`
- `selectors`：至少生成 3 个候选，按 `id` → `name` → `data-*` → 文本 → CSS path → XPath 顺序
- 每个 selector 都要**在生成时校验唯一性**：`document.querySelectorAll(sel).length === 1`，写进 `unique`
- `fragile`：命中以下任一即打标
  - class 含 `css-`、`scss-`、`_` 开头 + 8 位以上随机串（CSS-in-JS 特征）
  - id 含随机数字串
  - XPath 深度 > 8

#### 7.4.4 必须额外提取的页面级信息（`elements.json` 的 `page` 段）

| 字段 | 来源 |
|---|---|
| `ua` | `navigator.userAgent` |
| `x_ua_compatible` | `<meta http-equiv="X-UA-Compatible" content>` |
| `document_mode` | `document.documentMode`（Chromium 上应恒为 `undefined`） |
| `is_chromium` | UA 含 `Edg/` 或 `Chrome/` |
| `frameworks` | 检测 `window.jQuery` / `window.Vue` / `window.React` / `window.Ext` / `layui` / `element-ui` 等 |
| `has_show_modal_dialog` | `typeof window.showModalDialog === "function"` |
| `tab_structure` | 见下 |

**`tab_structure` 判定（Q4）**：

1. `Target.getTargets` 返回的 `type == "page"` 数量 > 1 → `"browser_tabs"`，记录每个 title/url
2. 否则若页面顶层 iframe 数 ≥ 4 且 iframe 的 `src` 或 `title` 含业务关键词 → `"iframes"`
3. 否则查是否存在 `[role=tablist]` 或 class 含 `tab` 的容器 ≥ 4 个 → `"dom_tabs"`
4. 否则 `"unknown"`

#### 7.4.5 控件语义映射

探针要把 SOP 里的中文控件名映射到具体元素，输出一张 `control_map`：

```json
{
  "下段扫描_邮件号码输入框": {"selector": "input[name='mailNo']", "attr_maxlength": null, "tag": "input"},
  "下段扫描_确定按钮":       {"selector": "//button[normalize-space()='确定']", "unique": false, "note": "页面上有 3 个'确定'按钮，需按祖先容器区分"},
  "确认下段按钮":            {"selector": "#confirmScan", "unique": true}
}
```

匹配算法：对每个目标中文词，在 `elements.json` 里找 `text`/`placeholder`/`title`/`aria-label`/关联 `<label>` 文本**精确等于**或**包含**该词的可见元素，按"离上一次点击位置最近"排序。**不准的必须标 `unique:false` 并写 note，不许猜。**

### 7.5 事件关联（`correlate`）

这是把"点击"和"网络请求"对上的模块，也是让报告可读的关键。

#### 7.5.1 注入点击监听

用 CDP 绑定机制：

```json
{"id":N,"method":"Runtime.addBinding","params":{"name":"__probe","executionContextId":<可选>},"sessionId":"<sid>"}
```

JS 侧（`record_click.js`，内容见 §7.5.2）通过调用 `window.__probe(JSON.stringify(payload))` 回传，Rust 侧订阅 `Runtime.bindingCalled` 事件（`params.name == "__probe"`）。

#### 7.5.2 `record_click.js` 内容

```js
(function () {
  if (window.__probeInstalled) return;
  window.__probeInstalled = true;

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return '';
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    let cur = el, depth = 0;
    while (cur && cur.nodeType === 1 && depth < 12) {
      let sel = cur.tagName.toLowerCase();
      if (cur.getAttribute && cur.getAttribute('name'))
        sel += "[name='" + cur.getAttribute('name') + "']";
      const p = cur.parentElement;
      if (p) {
        const idx = Array.prototype.indexOf.call(p.children, cur) + 1;
        sel += ':nth-child(' + idx + ')';
      }
      parts.unshift(sel);
      cur = p; depth++;
    }
    return parts.join(' > ');
  }

  function describe(el) {
    if (!el || el.nodeType !== 1) return null;
    const r = el.getBoundingClientRect();
    return {
      tag: el.tagName.toLowerCase(),
      id: el.id || null,
      name: el.getAttribute ? el.getAttribute('name') : null,
      cls: (el.className && typeof el.className === 'string') ? el.className.slice(0, 200) : null,
      type: el.getAttribute ? el.getAttribute('type') : null,
      text: (el.innerText || el.value || '').trim().slice(0, 80),
      placeholder: el.getAttribute ? el.getAttribute('placeholder') : null,
      rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
      css: cssPath(el)
    };
  }

  function send(type, extra) {
    try {
      if (typeof window.__probe === 'function') {
        window.__probe(JSON.stringify(Object.assign({
          type: type,
          ts: Date.now(),
          url: location.href
        }, extra)));
      }
    } catch (e) { /* 静默，绝不打断页面 */ }
  }

  document.addEventListener('click', function (e) {
    const raw = e.target;
    const el = (raw.closest && raw.closest('button,a,input,select,textarea,[role=button],[role=tab],li')) || raw;
    send('click', { element: describe(el), target_chain: [describe(el), describe(el && el.parentElement)] });
  }, true);

  document.addEventListener('change', function (e) {
    const el = e.target;
    if (!el) return;
    send('change', { element: describe(el), value_len: (el.value || '').length, value_preview: (el.value || '').slice(0, 30) });
  }, true);

  document.addEventListener('keydown', function (e) {
    if (e.key === 'Enter') {
      send('enter', { element: describe(e.target) });
    }
  }, true);

  send('installed', {});
})();
```

**约束：脚本必须永不抛错、永不阻止事件冒泡、永不改值。** 上面每个回调都包了 try/catch。

#### 7.5.3 时间窗口关联

Rust 侧维护一个 500 ms 的滑动窗口：
- 收到 `click` 事件 → 记 `t_click`
- 收到 `Network.requestWillBeSent` → 若 `now - t_click < 500ms`，把该 `requestId` 关联到最近的那次 click

产出关联表写进 `timeline.jsonl`，字段 `caused_by_click_seq`。

#### 7.5.4 键盘热键切换 step

主线程在 Windows 上注册全局热键 → 切换当前 step（A/B/C/D），另有一个"结束记录"热键。
用 `RegisterHotKey` + 消息循环（独立线程，`GetMessage` 循环）。

**热键表是可配置的，不要写死在代码里。** 内网机上 `Ctrl+Alt+F1..F4` 有可能被输入法、远程桌面、安全软件或业务客户端占用，而这件事在开发机上永远测不出来。因此：

```rust
// 默认值（放 cli.rs，可由 --hotkeys 覆盖或 config 文件覆盖）
struct HotkeySpec { combo: String, step: Step }   // 默认 "CTRL+ALT+F1" -> A, ... "CTRL+ALT+F9" -> END
```

四条必须实现的行为：

1. `RegisterHotKey` 的返回值**必须检查**。失败（通常是 `ERROR_HOTKEY_ALREADY_REGISTERED = 1409`）不能静默忽略。
2. **降级链**：`CTRL+ALT+F1..F4` → 全部或部分失败时依次尝试 `CTRL+SHIFT+F1..F4` → `CTRL+ALT+1..4`。按**整组**降级（不要一个键用第一套、另一个键用第二套，操作员会记错）。
3. 启动时把**实际生效**的热键表打印到 stdout 并写 `out/hotkeys.json`，让操作员卡上写的键和实际能用上的键可以对照。全部三套都失败时不要退出，打印"热键不可用，请用 `--step` 固定阶段或每阶段重开一次探针"，探针主体功能不受影响。
4. `RegisterHotKey` 必须带 `MOD_NOREPEAT (0x4000)`。不加的话操作员按住 F1 会连续触发几百次 step 切换，事件标签全乱。

`--hotkeys` 取值示例：`--hotkeys "ctrl+alt+f1=A,ctrl+alt+f2=B,ctrl+alt+f3=C,ctrl+alt+f4=D,ctrl+alt+f9=END"`。解析失败要报错退出（退出码 2），不要静默回退到默认值。

### 7.6 弹窗分类（`dialogs`）

每条记录必须归入以下**四类之一**：

| 类型 | 判定 | 能否自动化 |
|---|---|---|
| `dom_modal` | MutationObserver 发现新增节点匹配 `[role=dialog]`、`.el-dialog`、`.el-message-box`、`.modal`、`.ant-modal`、`position:fixed && z-index>1000` | ✅ 可以 |
| `js_dialog` | CDP 事件 `Page.javascriptDialogOpening` | ⚠️ 可以（必须 `Page.handleJavaScriptDialog` 才能放行） |
| `native_window` | 点击后 300 ms 内的 `EnumWindows` 快照增量，新 HWND 的 `GetWindowThreadProcessId` 属于 msedge.exe 进程树 | ❌ CDP 碰不到，需要 Win32 |
| `print_preview` | 同 `native_window`，但窗口 title 含 `打印` / `Print`，或 class 含 `Chrome_WidgetWin` 且是打印预览特征 | ❌ 需要 Win32 |

**实现顺序：**

1. 先 `Page.enable`，装 `Page.javascriptDialogOpening` 订阅。**注意：检测到该事件时必须立刻发 `Page.handleJavaScriptDialog {accept:true}`，否则页面冻结、操作员点不动。** 这个策略由 `--dialog-policy accept|cancel|leave` 控制，默认 `accept`。
2. 每个 page 注入 `mutation_probe.js`（监听 `childList`/`attributes`，节流 100ms，把新增的疑似弹窗节点发回 `__probe`）。
3. 独立线程每 500 ms 做一次 `EnumWindows` 全量快照，与上一帧 diff。要过滤掉已知的常驻窗口（任务栏、浏览器主窗口、输入法候选框）。
4. 三类结果按时间窗（100 ms）+ rect 重叠（≥ 60%）去重合并。

`dialogs.json` 每条记录附 `PrintWindow` 截图（§7.8.2）。

**必须特别处理：`page.on('dialog')` 期间不能发送任何 CDP 命令。** 给 CDP 命令通道加一个全局 `dialog_open: AtomicBool`，置位时新命令阻塞排队。

### 7.7 Q2 受控实验（`experiment`）

#### 7.7.1 静态检测（只读，safe）

注入执行：

```js
(function () {
  var inputs = Array.from(document.querySelectorAll('input,textarea'));
  return JSON.stringify(inputs.map(function (el) {
    var r = { id: el.id, name: el.name, tag: el.tagName, maxlength_attr: el.getAttribute('maxlength'), maxLength_prop: el.maxLength, type: el.type };
    // 程序化写入 30 个字符，读回实际长度
    var old = el.value;
    try {
      el.focus();
      el.value = '1'.repeat(30);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      r.after_write_len = el.value.length;
    } catch (e) { r.after_write_len = -1; }
    el.value = old;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return r;
  }));
})()
```

判定 H1：
- 有 `maxlength === "21"` 或 `maxLength === 21` → `H1` 成立
- `after_write_len === 21` 但无属性 → 框架层截断，也判 `H1`（`h1_subtype: "framework"`）
- `after_write_len === 30` → H1 不成立，进入下一步

#### 7.7.2 网络取证判定 H2（无需主动发请求）

从 `network.har` 里取所有和"下段扫描"关联的请求（靠 `caused_by_click_seq` 指向"确定"按钮）：

- 若**每次点击"确定"都恰好产生 1 个请求，且请求体只含 1 个单号** → **`H2` 成立**，`q2_verdict = "H2"`，`explanation`: "21 是操作员心理批次大小，系统无批量接口，每次确定处理 1 单"
- 若一次点击产生 1 个请求但 body 含多个单号 → 进 7.7.3
- 若无任何网络请求（纯前端）→ 记录并进 7.7.3

**这一步是纯旁路观测，不需要额外实验，优先级最高，先做。**

#### 7.7.3 协议重放实验（主动发请求，需确认）

前置条件：已捕获到至少 1 个"下段扫描"的完整请求模板（url / method / headers / body）。

**保护措施（逐条实现，一条都不能省）：**

1. 必须带 `--confirm-dry-run`，否则退出码 2 并打印：
   ```
   [ABORT] experiment 会发送真实业务请求，可能产生真实业务记录。
   请确认已在操作员知情的情况下执行，并已准备好测试用单号。
   确认后请加 --confirm-dry-run 重新运行。
   ```
2. 启动后在 stdout 打印倒计时 10 秒，可 Ctrl+C 中断。
3. **禁止使用真实待处理单号**，如果 body 里的单号出现在 `secrets/` 的白名单里，替换为 `--test-number` 参数指定的值。没指定 `--test-number` 则拒绝运行。
4. 只发 `N` 个变体（默认 21、22、30），串行，每个之间间隔 3 秒。
5. 每个响应记录：`status`、`body_preview`（前 2 KB）、`duration_ms`、`is_business_error`（body 含 `error`/`fail`/`失败`/`超过`/`上限` 等关键词，`code != 0` 等）。
6. 全部结束后生成人工可读的对比表。

**结论写入 `experiment21.json` 的 `q2_verdict`：**
- 22 成功且 30 成功 → `H2`（后端无限制）
- 22 失败 → `H3`，并记录报错文案和阈值
- 已有 `maxlength` → `H1`

#### 7.7.4 若 H2 成立，额外给出建议

报告里写一句：

> 建议后续程序按"单号循环、单请求"实现，无需批次概念；21 仅为人工操作的段落划分。

### 7.8 截图（`shot` + `win32`）

#### 7.8.1 页面截图

在每次 step 切换、每次弹窗出现时：

```json
{"id":N,"method":"Page.captureScreenshot","params":{
  "format":"png","captureBeyondViewport":true,"fromSurface":true
},"sessionId":"<sid>"}
```

Base64 解码后用 `image` crate 校验并写 PNG。文件名：`screenshots/page-<step>-<seq>-<ts>.png`。

**同时记录当前活动 target 的 url 和 title**，写进 `timeline.jsonl`，因为截图本身不含身份信息。

#### 7.8.2 原生窗口截图

```rust
// 关键：GetWindowRect → GetWindowDC → CreateCompatibleDC → CreateCompatibleBitmap
// → SelectObject → PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT=2)
// → GetDIBits（DIB_RGB_COLORS, 32bpp, 负高度）→ 校验 BPF 非全黑 → image::save_buffer
```

**三个必须点：**

1. `PrintWindow` 的 `nFlags` 必须传 `PW_RENDERFULLCONTENT = 2`，传 0 在 DWM 合成下大概率返回全黑。
2. `GetDIBits` 的 `biHeight` 传**负值**（top-down DIB），否则图像上下颠倒。
3. 拍完检测像素方差，若全黑则降级：把目标窗口 `SetForegroundWindow` → `Sleep(120ms)` → 用 `BitBlt` 从桌面 DC 拷。降级要在记录里写 `screenshot_method: "bitblt_fallback"`。

进程归属：`GetWindowThreadProcessId` → 拿 PID → 沿父进程链找 `msedge.exe`（用 `windows` crate 的 `CreateToolhelp32Snapshot`）。

#### 7.8.3 打印机信息（Q7）

**本节只做被动观测，绝不触发打印、绝不改变打印分支行为。**

`EnumPrintersW` 列所有打印机，`GetDefaultPrinterW` 取默认。
`EnumJobs`（对默认打印机）每 2 秒轮询一次，记录 `job_count` / `job_id` / `document` / `status` / `total_pages`。

写进 `report.json` 的 `print_env` 段，字段：

```json
{
  "printers": ["..."],
  "default_printer": "...",
  "is_virtual": true,
  "spool_events": [{"ts":"...","job_id":12,"document":"...","total_pages":1,"step":"B","nearest_click_seq":37,"delta_ms":840}]
}
```

**这一段的用途是回答 Q5/Q7，不是要改打印设置：**

- 若操作员点"确定打印"之后 `spool_events` 出现新 job → 那一步**真的会出纸**，正式自动化程序必须绕开它（见下）。
- 若点完之后 3 秒内没有任何 job → 打印分支是"只弹预览/或后端异步"，正式程序只需关掉预览窗。
- `is_virtual`（默认打印机是 `Foxit PDF`/`Microsoft Print to PDF`/`Adobe PDF` 之类）为 true 时，说明现场本来就是出 PDF 不出纸，Q7 直接结案。

**关于 `--disable-print-preview` 和 `--kiosk-printing`：**

这两个参数**不放进探针，也不要给 Edge 用**。理由不是"可能出纸"，而是它们会**破坏取证**：
- `--disable-print-preview` 直接跳过预览，而"打印预览窗口"（`print_preview` 类原生窗口，§7.6）正是 Q5 要判定的对象 —— 开了它，目标分支就消失了，等于把要观测的现象关掉再观测。
- `--kiosk-printing` 自动确认对话框并直接送 spooler，会真的出纸。

后续自动化阶段的正确做法也不是这两个参数，而是 **CDP 的 `Page.printToPDF`**：它返回 PDF 字节流，完全不经过 spooler、不弹任何窗口，探针只需确认业务侧是否接受"PDF 落地文件"作为打印产物（这属于 §2 Q5 的组织流程问题，报告里要点出来）。该能力写进 §16 备选，本次不实现。

### 7.9 单实例锁与续跑（`session`）

1. `out/.lock` 文件 + `CreateFileW` 带 `FILE_SHARE_READ`（独占）实现单实例。已被占用则退出码 3，打印占用进程 PID。
2. 每次启动建 `out/sessions/<YYYYMMDD-HHMMSS>/`，把 `out/latest` 写成指向它的一个文本文件（存相对路径，避免符号链接权限问题）。
3. `timeline.jsonl` 每事件一行，写完立刻 `flush` + `sync_data`（宁可慢，不许丢）。
4. `report` 命令扫描所有 session 目录，按时间合并。
5. 崩溃钩子：`std::panic::set_hook` 里把当前 step、最后 50 条事件、调用栈写入 `out/sessions/<ts>/crash.txt`。

---

## 8. 数据结构（逐字段规定）

### 8.1 `timeline.jsonl` 事件

每行一个 JSON 对象，公共字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `seq` | u64 | 全局自增 |
| `ts` | string | ISO8601 本地时间，毫秒 |
| `ts_mono` | u64 | 启动后毫秒数（用于精确排序） |
| `kind` | string | 见下表 |
| `step` | string? | `A` / `B` / `C` / `D` / null |
| `target` | string? | targetId |
| `session` | string? | CDP sessionId |

`kind` 取值与附加字段：

| kind | 附加字段 |
|---|---|
| `preflight` | `checks: [{name, ok, detail}]` |
| `target_attached` | `target_id, type, url, title` |
| `click` | `element: ElementDesc, target_chain: [ElementDesc]` |
| `change` | `element, value_len, value_preview` |
| `enter` | `element` |
| `request` | `request_id, method, url, post_data, headers, plaintext, caused_by_click_seq` |
| `response` | `request_id, status, headers, mime, encoded_len, body_preview, body_status` |
| `dialog_dom` | `selector, text_preview, rect, z_index` |
| `dialog_js` | `message, dialog_type` |
| `dialog_native` | `hwnd, class, title, pid, exe, rect, screenshot` |
| `screenshot` | `path, w, h, method` |
| `assertion` | `id, expected, observed, pass` |
| `note` | `text`（人工/热键插入的标记） |
| `error` | `module, message` |

`ElementDesc` 结构见 §8.2 的单个元素。

### 8.2 `elements.json`

```json
{
  "generated_at": "2026-11-17T14:03:22.181+08:00",
  "page": {
    "url": "...",
    "title": "...",
    "ua": "...",
    "is_chromium": true,
    "x_ua_compatible": null,
    "document_mode": null,
    "frameworks": ["Vue"],
    "has_show_modal_dialog": false,
    "tab_structure": "browser_tabs",
    "tab_targets": [{"id": "...", "title": "...", "url": "..."}],
    "elements_truncated": false
  },
  "frames": [
    {
      "frame_id": "top",
      "url": "...",
      "elements": [
        {
          "seq": 1,
          "tag": "input",
          "text": "",
          "placeholder": "请输入邮件号码",
          "aria_label": null,
          "label_text": "邮件号码",
          "attrs": {"id": "mailNo", "name": "mailNo", "class": "el-input__inner", "type": "text", "maxlength": null, "disabled": false, "readonly": false},
          "visible": true,
          "enabled": true,
          "rect": [120, 340, 260, 32],
          "z_index": null,
          "in_shadow": false,
          "selectors": [
            {"type": "id",    "value": "#mailNo",  "unique": true},
            {"type": "name",  "value": "input[name='mailNo']", "unique": true},
            {"type": "xpath", "value": "//label[normalize-space()='邮件号码']/following::input[1]", "unique": true}
          ],
          "fragile": []
        }
      ]
    }
  ],
  "control_map": {
    "下段扫描_邮件号码输入框": {"selector": "#mailNo", "unique": true, "attr_maxlength": null},
    "下段扫描_确定按钮": {"selector": null, "unique": false, "note": "同文本按钮 3 个，无法唯一确定，需人工确认"}
  }
}
```

### 8.3 `dialogs.json`

```json
{
  "generated_at": "...",
  "dialogs": [
    {
      "seq": 1,
      "ts": "...",
      "step": "B",
      "type": "dom_modal",
      "trigger_click_seq": 42,
      "matched_request_ids": ["1234.5"],
      "evidence": {
        "selector": "body > div.el-dialog__wrapper:nth-child(9)",
        "text_preview": "提示：该邮件号已完成下段扫描",
        "rect": [480, 300, 400, 180],
        "hwnd": null,
        "exe": null,
        "screenshot": "screenshots/dialog-0001.png"
      },
      "close_method": "click_x",
      "close_selector": "div.el-dialog__wrapper:nth-child(9) button.el-dialog__headerbtn"
    }
  ],
  "classifications": {
    "dom_modal": 12,
    "js_dialog": 0,
    "native_window": 2,
    "print_preview": 1
  }
}
```

### 8.4 `experiment21.json`

```json
{
  "generated_at": "...",
  "static_check": {
    "inputs": [{"id": "mailNo", "maxlength_attr": null, "maxLength_prop": 524288, "after_write_len": 30}],
    "h1_verdict": "not_applicable"
  },
  "network_check": {
    "scan_requests": 21,
    "numbers_per_request": 1,
    "h2_verdict": "supported"
  },
  "replay_check": {
    "executed": false,
    "reason": "h2 已成立，无需主动发请求",
    "results": []
  },
  "q2_verdict": "H2",
  "explanation": "每次点'确定'只发送 1 个单号，21 为人工操作段落，系统无批量接口。",
  "implication": "后续程序按单号循环实现，无需批次概念。"
}
```

`replay_check.executed == true` 时 `results` 结构：
```json
[{"n": 21, "status": 200, "duration_ms": 312, "body_preview": "...", "is_business_error": false}]
```

### 8.5 `assertions.json`

SOP 末尾的成功判据转成机器断言：

```json
{
  "assertions": [
    {
      "id": "A_scan_confirm",
      "step": "A",
      "trigger": "点击'确认下段'",
      "expected": "出现成功提示（弹窗或页面提示）",
      "expected_request_url_contains": ["scan", "confirm"],
      "observed": "dom_modal seq=7",
      "pass": true
    },
    {
      "id": "B_print_phone_loop",
      "step": "B",
      "trigger": "点击'打印'",
      "expected": "每轮弹出 1 个手机号输入框，直到不再弹出",
      "observed": "共出现 4 次 dom_modal，输入框 selector 稳定",
      "pass": true
    },
    {
      "id": "B_rescan_success",
      "step": "B",
      "trigger": "再次点击'下段扫描'并输入邮件号",
      "expected": "弹出框体",
      "observed": "dom_modal",
      "pass": true
    },
    {
      "id": "D_handover_done",
      "step": "D",
      "trigger": "点击 '->'",
      "expected": "流程结束态",
      "observed": "页面跳转 / 列表清空",
      "pass": true
    }
  ]
}
```

### 8.6 `replay/scenario.json`

从 `network.har` 自动提取，去掉静态资源和埋点，按时间排序：

```json
{
  "generated_at": "...",
  "base": "http://<host>",
  "steps": [
    {
      "step": "A",
      "label": "下段扫描-输入单号",
      "trigger": "click 确定 (element seq 12)",
      "method": "POST",
      "path": "/api/xxx/scan",
      "headers": {"Content-Type": "application/json", "X-Token": "<masked>"},
      "body_template": "{\"mailNo\":\"{{MAIL_NO}}\",\"ts\":\"{{TS}}\"}",
      "dynamic_fields": ["ts"],
      "response_shape": "{\"code\":0,\"msg\":\"成功\"}"
    }
  ],
  "encryption_notes": "若 headers/body 出现疑似签名或密文，在此说明"
}
```

**`dynamic_fields` 是重点**：若 body 里有随机的 `ts`、`nonce`、`sign`，必须识别出来并标注，因为这直接决定协议回放可行性（对应 Q1）。

识别规则：
- 字段名匹配 `^(ts|time|timestamp|nonce|sign|signature|token|_t|rand|random)$`（不区分大小写）
- 或值在 3 次请求中均不相同且长度 > 8

---

## 9. 报告格式

### 9.1 `report.md` 骨架（**标题层级和顺序不得改动**）

```markdown
# 退货流程自动化探针报告

> ⚠️ 本报告及同目录文件可能包含账号凭据与业务数据，仅限内网留存，禁止外传。
> 凭据完整值见 secrets/secrets.json

生成时间：<ts>
会话数：<n>
Edge 版本：<ver>
页面 UA：<ua>

---

## 一、一句话结论

<必填三选一，后附 1 句理由>
- 建议走【协议回放】：请求明文可复现，无签名
- 建议走【UI 自动化】：请求有签名/加密，只能操作界面
- 建议【混合】：查询类走协议，写入类走 UI

## 二、Q1 请求取证
| 步骤 | 方法 | 路径 | 协议 | 是否含签名 | 备注 |

## 三、Q2 关于"21"
判定：<H1|H2|H3|unknown>
依据：<...>
对后续程序的影响：<...>

## 四、Q3 控件定位器
<表格：控件名 | 主定位器 | 唯一性 | 备选 | 脆弱性>

## 五、Q4 页面结构
判定：<browser_tabs|iframes|dom_tabs|unknown>
详情：<...>

## 六、Q5 打印分支判定
分支B1 触发条件：<推测 + 证据>
分支B2 触发条件：<推测 + 证据>
点"确定打印"后是否真的进 spooler：<是：job_id=N, 页码=M | 否：3 秒内无 job>
置信度：<高|中|低>

## 七、Q6 性能
| 步骤 | 单次耗时 | 500 单外推 | 瓶颈 |

## 八、Q7 打印环境
默认打印机：<...>（是否虚拟打印机：<是|否>）
检测到的打印窗口：<...>
点"确定打印"后 spooler 事件：<...>
可行绕过方案：<...>

## 九、弹窗清单
| # | 步骤 | 类型 | 触发 | 关闭方式 | 定位器 |

## 十、风险清单
1. ...

## 十一、后续三个月路线建议
<阶段化，每阶段有可验证判据>

## 十二、附录
- 证据文件索引
- 未解决的问题
```

**规则：**
- 任何一节没有数据，必须写 `未获取到（原因：...）`，**不许留空，不许编造**
- 置信度必须逐条给出，没有依据的写"低"
- 报告里出现的所有单号/手机号必须已脱敏

### 9.2 `report.json`

同样的内容，机读版。顶层 key 固定：

```json
{
  "meta": {...},
  "verdict": {"path": "protocol|ui|hybrid", "reason": "..."},
  "q1": {...}, "q2": {...}, "q3": {...}, "q4": {...},
  "q5": {...}, "q6": {...}, "q7": {...},
  "dialogs": [...],
  "risks": [...],
  "roadmap": [...]
}
```

---

## 10. 脱敏规则（`redact`）

| 数据 | 规则 | 例 |
|---|---|---|
| 邮件号（13 位数字） | 保留前 4 位，其余替换为 `*` | `710000000004161` → `7100*********` |
| 手机号 | 保留前 3 后 2 | `13000000000` → `130******00` |
| 密码类字段名匹配 `pass\|pwd\|secret\|token\|cookie\|authorization\|sign` | 整个值替换为 `<REDACTED:len=N>` | |
| Cookie header | 整个值置 `<REDACTED>`（除非 `--include-cookies`） | |
| 收件人姓名/地址 | 保留首字 + `*` | `张三` → `张*` |

**实现要求：**
- 脱敏函数必须是**纯函数**，输入输出都测试过（写单元测试）
- 正则预编译（`once_cell::Lazy`）
- `secrets/secrets.json` 存完整值，文件名和目录名固定，权限设为仅当前用户可读（`icacls`）

---

## 11. 本地模拟页（`mock/`）—— DeepSeek 自测用

因为没有内网访问权限，**必须**先做这个，否则无法验证探针本身正确。

### 11.1 要求

一个纯静态 HTML + JS，模拟四个阶段，包含：

1. 4 个可切换的 tab（DOM tab，模拟 Q4 的一种情况）
2. 阶段A：一个 `input[name=mailNo]`（**不带 maxlength**，用于验证 H2 判定路径）+ 一个"确定"按钮，点击后 `fetch('/api/scan', {method:'POST', body: JSON.stringify({mailNo: v})})`，然后弹出一个 DOM 模态框
3. 一个也带 `maxlength="21"` 的隐藏备用输入框（用 URL 参数 `?mode=h1` 切换），用于验证 H1 判定路径
4. 阶段B：一个"打印"按钮，点第一次弹 `alert`（验证 `js_dialog`），点第三次弹一串 DOM 模态框（验证批量分支）
5. 阶段C：一个按钮弹 `confirm`
6. 阶段D：正常流程

用一个极简 Rust 静态服务器（`axum` 可选，但更简单的是 tokio + 手写 HTTP 响应）监听 `127.0.0.1:8080`，或用 Python `http.server`（仅开发期用，不进交付物）。

### 11.2 验证清单（DeepSeek 自测必须全过）

- [ ] `probe attach` 能连上 mock 页
- [ ] `elements.json` 里 `input[name=mailNo]` 的 3 个 selector 全部 `unique: true`
- [ ] `network.har` 里能看到 POST `/api/scan`，`post_data` 正确
- [ ] `click` 事件和 `request` 通过 `caused_by_click_seq` 正确关联
- [ ] `?mode=h1` 时 `experiment21.json` 的 `q2_verdict == "H1"`
- [ ] 默认模式时 `q2_verdict == "H2"`（每次点击 1 个请求 1 个单号）
- [ ] `alert` 被归为 `js_dialog` 且页面不冻结
- [ ] DOM 模态框被归为 `dom_modal` 且有截图
- [ ] `report.md` 十二节全齐，无空节
- [ ] 断点续跑：跑一半 Ctrl+C，重启后 `latest` 指向同一 session 且事件不重复
- [ ] `probe.exe report` 能从空 session 目录重建报告不 panic

---

## 12. 操作员操作卡（`operator-card.md`，交付物之一）

内容要点（写成人话，给不会用命令行的人看）：

```
第 0 步：双击 probe.exe preflight，看到全部 [OK] 再继续。
        有任何 [FAIL]，把窗口截图发给技术。
第 1 步：双击 启动Edge.bat（探针附带生成），浏览器会自动打开。
        在这个浏览器窗口里登录邮政平台，打开 4 个标签页。
        ⚠️ 不要关掉这个窗口后重开，重开会导致探针断开。
第 2 步：技术会在后台启动 probe.exe attach。
        启动后按 Ctrl+Alt+F1，屏幕无变化就说明连上了。
        ⚠️ 如果命令行窗口里打印的热键不是 Ctrl+Alt+F1（比如变成了
           Ctrl+Shift+F1），以打印出来的为准 —— 说明有别的软件占用了默认键。
第 3 步：按平时的操作顺序做，每换一个阶段按一次热键：
        Ctrl+Alt+F1 = 下段扫描
        Ctrl+Alt+F2 = 详情单补打
        Ctrl+Alt+F3 = 退回寄件人 / 妥投确认
        Ctrl+Alt+F4 = 投递归班交接
第 4 步：整个过程做 3-5 个真实单号就够，不用做完全部。
        做完后按 Ctrl+Alt+F9 结束记录。
第 5 步：把 D:\probe\out 整个目录打包发给技术。
        ⚠️ 包含账号信息，走内网传输，不要用微信/邮箱。
注意事项：
- 弹框出现时正常处理，探针只记录，不会干预你的操作。
- 如果探针的黑色命令行窗口闪退，把 out\session* 下的 crash.txt 发出来。
```

---

## 13. 实施顺序（每步都有独立验收，不要跳）

| Step | 内容 | 验收 |
|---|---|---|
| 1 | Cargo 工程骨架 + CLI + `preflight` | `probe.exe preflight` 能打印所有检查项，无 panic |
| 2 | `mock/` 静态页 + 本地服务 | 浏览器能打开并走完模拟流程 |
| 3 | CDP 客户端（`cdp/`）+ `attach` 连上并列出 targets | 对 mock 页能列出 1 个 page target |
| 4 | `netcap` + HAR 导出 | mock 的 `/api/scan` 出现在 HAR 里 |
| 5 | `domino` + `inspect_dom.js` | mock 页的元素清单正确、selector 唯一 |
| 6 | `correlate` + `record_click.js` + binding | 点击与请求关联正确 |
| 7 | `dialogs`（dom_modal + js_dialog 两类） | mock 的 alert 和 modal 都正确分类 |
| 8 | `experiment` 静态检测 + 网络判定（H1/H2） | mock 两种模式判定正确 |
| 9 | `report`（markdown + json + assertions） | 十二节齐全，无空节 |
| 10 | `win32`（窗口枚举 + PrintWindow + 打印机） | 能截图 Edge 主窗口并列出打印机 |
| 11 | `experiment` 协议重放（带所有保护） | 在 mock 上能跑通，`--confirm-dry-run` 缺失时正确拒绝 |
| 12 | `session` 续跑 + 崩溃钩子 + 单实例锁 | 自测清单一节全过 |
| 13 | `redact` + `secrets` | 单元测试通过，报告里无明文单号 |
| 14 | 打包：`cargo vendor` + README + 操作卡 | 在**另一台干净 Win11** 上 `cargo build --offline` 成功 |

---

## 14. 最终验收标准

探针在内网机运行一次后，产出的 `report.md` 必须满足：

- [ ] 第一节能给出明确的 `协议回放 / UI 自动化 / 混合` 之一，并有理由
- [ ] Q1–Q7 每一条都有结论 + 证据文件路径 + 置信度
- [ ] `q2_verdict` 明确（H1/H2/H3），不允许 `unknown` 作为最终答案（若确为 unknown，必须写明"未测出，建议补做 §7.7.3"）
- [ ] `elements.json` 中 `control_map` 覆盖 SOP 里出现的**全部**控件名（连同义变体），无法唯一确定的必须标 `unique:false` + note
- [ ] `dialogs.json` 中每一类弹窗至少 1 个样本，且有截图
- [ ] `replay/scenario.json` 至少覆盖阶段A 和阶段C 的最小请求序列
- [ ] 报告里**不出现**未脱敏的单号和手机号
- [ ] 全程无外网依赖，单 exe 可直接运行
- [ ] 崩溃时留下 `crash.txt`，不静默退出

---

## 15. 已知坑与禁令

### 15.1 已知坑（按踩到概率排序）

1. **9222 端口不监听** → 一定是没加独立 `--user-data-dir`，或本机已有 Edge 实例
2. **WebSocket 403** → 漏了 `--remote-allow-origins=*`
3. **`Page.javascriptDialogOpening` 之后页面卡死** → 没发 `Page.handleJavaScriptDialog`
4. **截图全黑** → `PrintWindow` 没传 `PW_RENDERFULLCONTENT`
5. **截图上下颠倒** → `GetDIBits` 的高度没传负值
6. **中文乱码** → 没调 `SetConsoleOutputCP(65001)` 和 `SetConsoleCP(65001)`
7. **`Network.getResponseBody` 报错** → 资源已回收，属正常，必须容忍
8. **企业策略禁用远程调试** → `preflight` 必须提前发现，否则操作员会白跑
9. **`elements.json` 抓不到东西** → 页面在 iframe 里，`Target.setAutoAttach` 没配上
10. **时间对不上** → 用 `ts_mono`（单调时钟）而不是 `ts`（墙钟）做排序和窗口计算

### 15.2 禁令（违反即返工）

- ❌ 不要自动点击业务按钮（除 `--confirm-dry-run` 实验）
- ❌ 不要修改页面 DOM 或表单值（`experiment` 静态检测里的写入必须立即还原）
- ❌ 不要把账号密码写进源码、写进 exe、写进日志
- ❌ 不要引入需要 C/C++ 工具链或 cmake 的 crate
- ❌ 不要用无头模式（`--headless`）
- ❌ 不要 kill 浏览器进程
- ❌ 不要在报告里编造数据：没采到就写"未获取到"
- ❌ 不要把 `secrets/` 的内容写进 `report.md`
- ❌ 不要硬编码窗口坐标或屏幕分辨率
- ❌ 不要依赖 `out` 目录之外的任何路径
- ❌ 不要给 Edge 加 `--disable-print-preview` / `--kiosk-printing` / `--headless`（前两个会破坏 Q5 取证或真的出纸，理由见 §7.8.3）
- ❌ 不要用 `chromiumoxide` 或其他 CDP 封装 crate（理由见 §3.2）
- ❌ 热键不要写死：必须走 §7.5.4 的配置表 + 注册失败降级链

### 15.3 编码细节要求

- 所有面向用户的中文输出用 UTF-8，控制台先 `SetConsoleOutputCP(65001)`
- 所有 `unwrap()` 只允许出现在 `main` 里，其余用 `?` + `anyhow::Context` 附加中文上下文
- 每个模块顶部写 `//!` 文档注释，说明该模块回答 §2 的哪个问题
- 所有 CDP 命令必须有 10 秒超时（`tokio::time::timeout`），超时记 `error` 事件后继续，不 panic

---

## 16. 扩展预留（本次不实现，但结构上不要堵死）

后续正式程序会复用本工程，因此：

- `cdp/`、`netcap.rs`、`domino.rs`、`dialogs.rs`、`redact.rs` 必须设计成**可被库引用**，不依赖 `main`
- 建议拆成 `src/lib.rs` + `src/bin/probe.rs`，`Cargo.toml` 里同时声明 `[lib]` 和 `[[bin]]`
- 事件结构体都加 `#[derive(Serialize, Deserialize)]`，方便后续程序直接反序列化历史数据
- `experiment.rs` 里识别出的签名/动态字段逻辑，后续要能直接被"协议回放客户端"复用
- **打印链路备选**：正式程序优先走 CDP `Page.printToPDF`（返回 PDF 字节流，不经 spooler、不弹窗），
  探针的 `print_env.spool_events` 结果将决定这条路线是否够用；若业务要求必须走系统打印机，
  才考虑 §7.8.3 明确排除的 Chromium 打印参数 —— 且必须在正式程序阶段另行验证，探针里不实现
- **热键配置化**已在本期实现（§7.5.4），后续正式程序可直接复用同一张热键表
