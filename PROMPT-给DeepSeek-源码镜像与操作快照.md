# 任务：把探针从「诊断器」改造成「影印机」

> 给 AI 编码助手的施工 prompt。你面对的是一个已交付、6072 行、31 个测试全绿的 Rust 工程。
> **这不是加功能，是改定位。** 读完 §0 再动手。

---

## 0. 为什么推倒重来

现有探针的定位是**诊断器**：回答 Q1–Q7，产出一份报告。
这个定位在"能反复进内网"的前提下是对的。

**但内网只能进一次。**

诊断器回答完就废了。第二趟内网机几乎不可避免——因为 DeepSeek 拿到报告后，还是要"再进去试一次"才知道代码对不对。

**新定位：影印机。** 探针必须把整个系统的**可离线复现形态**搬出来：

- 全部前端源码（HTML / JS / CSS / 运行时生成的脚本）
- 每个接口的完整请求与响应
- **每次操作前后的页面画面**
- 单号与操作的对应关系

目标只有一个：

> **拿到包的人，能在外网把自动化代码写完、在本地 `mock/` 上调通，内网那一趟只是"换地址重跑"。**

拿不到这个目标，第二趟内网机就是必然的。

---

## 1. 现状核实（已核对源码，不要重新调研）

### 1.1 已有的、直接复用

| 能力 | 位置 | 状态 |
|---|---|---|
| CDP 命令调用 | `cdp::Cdp::call` / `call_browser` | ✅ 通用，任意方法可发 |
| 取响应体 | `commands::get_response_body` | ✅ 已存在，**但只对 XHR 用** |
| 页面截图 | `commands::capture_screenshot`（支持 clip） | ✅ 已存在 |
| 整页截图 | `shot::page_shot(cdp, sid, sess, tag)` | ✅ |
| 元素枚举 | `domino` + `assets/inspect_dom.js` | ✅ |
| 事件出口 | `App::emit(kind, extra) -> seq` | ✅ |
| 时间线重放 | `NetStore::ingest_timeline` / `DialogStore::ingest_timeline` | ✅ |
| 脱敏 | `redact::redact_text` / `mask_number` | ✅ |
| 已开 CDP 域 | `Network` `Page` `Runtime` `DOM`（`cdp/mod.rs::enable_domains`） | ✅ |
| 点击 DOM 绑定 | `Runtime.addBinding("__probe")` + `assets/record_click.js` | ✅ |

### 1.2 现在缺的（**这就是本次要做的事**）

实测证据：一次完整 mock 会话共 **29 次 click，只有 6 张截图**。

| 缺口 | 现状 | 后果 |
|---|---|---|
| **源码镜像** | `Debugger.*` / `getResourceTree` / `captureSnapshot` **0 命中** | 拿回来的是残页，写不出自动化 |
| **操作前后截图** | 只在切阶段、弹窗时截 | 看不到"点一下发生了什么" |
| **完整 DOM** | `outerHTML` 每阶段一次，`truncate_bytes(2MB)` 从中间切断 | 一份 3.6 KB 的 DOM 在 tag 中间被截断，不是合法 HTML |
| **非 XHR 资源正文** | JS/CSS/图片从不取 body | 无法离线复现页面 |
| **数据入口** | 无任何"喂数据"参数 | 操作员只能手敲；单号与操作无关联 |
| **操作语义** | 只有 A/B/C/D 四个热键 | 粒度太粗，不知道自己点的是 SOP 第几步 |

---

## 2. 本次交付的四个模块

按**必须完成的顺序**。前一个没通过验收，不要开始下一个。

| # | 模块 | 文件 | 跑在哪 | 一句话 |
|---|---|---|---|---|
| 1 | 全站源码镜像 | `src/mirror.rs` | 内网机 | 把整个站点搬到磁盘上 |
| 2 | 操作快照带（filmstrip） | `src/filmstrip.rs` | 内网机 | 每次操作前后各一张图 + 完整 DOM |
| 3 | 数据入口 | `src/inputs.rs` | 内网机 | 单号清单、固定值、输入框值识别 |
| 4 | 操作语义标注 | `src/annotate.rs` | 内网机 | 每次操作打上 SOP 动作编号 |
| **5** | **AGENT PACK 编译器** | `src/agentpack/` | **开发机** | 把原始数据编译成 agent 能直接吃的语料 |

**前四个是"抓"，第五个是"编译"。** 这个分工是刻意的，见 §6A.1。

**上一轮那份「覆盖率与反馈包」prompt 暂停。** 等这四个做完，覆盖率才有真正的判据——那时候"缺"的定义是"缺一个接口的响应体"或"缺某个 JS 文件"，而不是"缺一次点击"。

---

## 3. 模块一：全站源码镜像（`src/mirror.rs`）

### 3.1 目标

会话结束时，磁盘上有一个目录：

```
mirror/
├── <host>/
│   ├── index.html              ← 原路径结构保留
│   ├── static/js/app.3f5c1d2.js
│   ├── static/css/index.css
│   └── api/...
├── inline_scripts/            ← 无 URL 的内联/动态脚本
│   ├── script-0001.js
│   └── script-0002.js
└── INDEX.json                 ← 每个资源的元数据
```

**验收标准：把 `mirror/<host>/index.html` 用浏览器以 `file://` 打开，页面骨架能渲染出来**（接口数据当然没有，但结构、样式、脚本加载顺序都在）。

### 3.2 四条采集途径（全部要实现）

#### (a) MHTML 整页存档 —— 最快拿到"完整页面"

```rust
// Page.captureSnapshot 返回 {"data": "<mhtml 文本>"}
let r = cdp.call("Page.captureSnapshot", json!({"format": "mhtml"}), Some(sid)).await?;
```

- 每个业务页面（A/B/C/D 四个 tab）各存一份：`mirror/mhtml/<step>-<ts>.mhtml`
- **MHTML 是自包含的**（HTML + 内联资源 + base64 图片），可以脱离原系统离线打开
- 这是**兜底**：万一 §3.2(c) 的资源抓取有遗漏，MHTML 里还有一份
- 中文文件名/内容注意编码：MHTML 是 `quoted-printable` 或 `base64`，**原样落盘，不要解码**

#### (b) 资源树全量遍历 —— 拿到 URL 清单

```rust
// Page.getResourceTree 返回 frameTree，含每帧的 resources[]
// resources[] 每项: {url, type, mimeType}
let tree = cdp.call("Page.getResourceTree", json!({}), Some(sid)).await?;
```

对每个 frame（递归 `childFrames`），取全部 `resources`。
这一步先把**URL 全集**拿到手，再去重——避免重复抓同一个文件。

#### (c) 资源正文 —— 抓 JS/CSS/图片

两个来源，**都要用，按优先级**：

1. **`Page.getResourceContent`**（推荐，不依赖 requestId）
   ```rust
   // 返回 {"content": "...", "base64Encoded": bool}
   let r = cdp.call("Page.getResourceContent",
       json!({"frameId": frame_id, "url": url}), Some(sid)).await?;
   ```
   - 适合 `Document` / `Script` / `Stylesheet` / `Image`
   - 已缓存资源也能取到

2. **`Network.getResponseBody`**（后备，用已捕获的 `requestId`）
   - 现有 `netcap` 已经给每个请求记了 `request_id`
   - 对 `type` 属于 `Script` / `Stylesheet` / `Image` / `Font` / `Document` 且 `status < 400` 的，补取
   - **现在的 `netcap` 只对 XHR/Fetch 取 body，这是要改的核心点**

**落盘规则：**

| 资源类型 | 处理 |
|---|---|
| JS / CSS / HTML | 原文落盘，按 URL 路径映射到 `mirror/<host>/...` |
| 图片 / 字体 | 二进制落盘，**大于 512 KB 的跳过**并记 `skipped: "too_large"` |
| `data:` URI | 跳过（已内嵌） |
| `blob:` URI | 跳过并记 `skipped: "blob"` |
| 跨域资源（不同 host） | 落盘到 `mirror/<other_host>/...`，保留区分 |

**路径映射必须防目录穿越**：URL 里的 `..`、绝对路径、Windows 保留名（`CON`/`PRN`/`AUX`/`NUL`/`COM1`）全部转义。
实现：`url_to_path(url) -> PathBuf`，单元测试覆盖 `http://a/b/../../etc/passwd`、`CON`、含中文、含 `?query` 的情况（query 丢弃但要记在 INDEX.json）。

#### (d) 运行时脚本 —— 抓不到 URL 的那些

很多内网系统用 `eval` 或动态 `<script>` 拼代码。这些**没有 URL**，只能从调试器拿。

```rust
// 1) 开启
cdp.call("Debugger.enable", json!({"maxScriptsCacheSize": 50000000}), Some(sid)).await?;

// 2) 监听 Debugger.scriptParsed 事件，收集 scriptId
//    事件字段: scriptId, url, startLine, startColumn, endLine, endColumn, sourceMapURL, isLiveEdit

// 3) 对每个 scriptId 取源码
let r = cdp.call("Debugger.getScriptSource", json!({"scriptId": id}), Some(sid)).await?;
// 返回 {"scriptSource": "..."}
```

**必须处理的坑：**

1. **`Debugger.enable` 会让页面变慢**，且部分系统有反调试检测（`debugger` 语句导致卡顿）。
   - 实现为**可关闭**：`--no-debugger`
   - 检测到页面明显卡顿（某次 `evaluate` 超过 5 秒）时自动关闭并记一条 `warn` 事件
2. **`scriptParsed` 会重复触发**（同一脚本多次解析），按 `scriptId` 去重
3. **没有 URL 的脚本**（`url` 为空）存到 `mirror/inline_scripts/script-<seq>.js`，并在 INDEX.json 里记 `parent_url`（哪一页触发）
4. **有 sourceMap 的**（`sourceMapURL` 非空）额外记一笔，**不去抓 map**（通常是外网 CDN，内网没有）

#### (e) 内联脚本兜底（`Debugger` 不可用时）

```js
// Runtime.evaluate，一次拿全部内联 <script>
Array.from(document.querySelectorAll('script:not([src])'))
     .map((s, i) => ({ i, len: s.textContent.length, text: s.textContent }))
```

结果存 `mirror/inline_scripts/eval-<seq>.js`。

### 3.3 `INDEX.json`

```json
{
  "host": "10.x.x.x:8080",
  "captured_at": "2026-11-17T15:30:12+08:00",
  "mhtml": [{"step": "A", "path": "mhtml/A-1794898402.mhtml"}],
  "resources": [
    {
      "url": "http://10.x.x.x:8080/static/js/app.3f5c1d2.js",
      "type": "Script",
      "mime": "application/javascript",
      "path": "10.x.x.x_8080/static/js/app.3f5c1d2.js",
      "size": 812344,
      "sha256": "...",
      "source": "page_getResourceContent",
      "status": 200
    },
    {"url": "...", "skipped": "too_large", "size": 2098123}
  ],
  "inline_scripts": [
    {"path": "inline_scripts/script-0001.js", "size": 3041, "source": "debugger", "parent_url": "..."}
  ],
  "summary": {"total": 42, "captured": 38, "skipped": 4, "bytes": 8123445}
}
```

**要求：**
- `resources` 按 `url` 字典序排序，稳定
- 每条都记 `source`（`page_getResourceContent` / `network_getResponseBody` / `debugger` / `eval`），便于判断可信度
- 失败资源**必须出现**在数组里（带 `skipped` 原因），不能静默丢弃
- `sha256` 用已有 `sha2`

### 3.4 脱敏策略（**和主产物不同，仔细读**）

主目录下的 `report.md` 等**已脱敏**。`mirror/` 下的源码**默认不脱敏**——因为脱敏会破坏 JS 语法、让代码不可读。

但源码里**可能硬编码了账号、密钥、接口签名盐**。

**规定：**

| 位置 | 是否脱敏 |
|---|---|
| `mirror/**` 本体 | ❌ 不脱敏（保持可运行、可读） |
| `mirror/INDEX.json` | ✅ 脱敏（只含 URL 和路径，不含内容） |
| `secrets/raw/` | ❌ 不脱敏，**这就是它的用途** |

**并且：**
- 在 `00-先读我.txt` 里写明"`mirror/` 含原始前端源码，可能含硬编码凭据，禁止外传"
- `bundle` 时 `mirror/` **默认打包**（这是核心价值），但 `secrets/raw/` 默认**不打**，与现有 `secrets/` 策略一致
- 加一个 `probe mirror-scan --out <DIR>` 子命令：**只读扫描** mirror 里的疑似凭据（`password`、`secret`、`appKey`、`sign`、`token`、`salt` 等键名），输出一份 `mirror/FINDINGS.md` 给人工复核

### 3.5 磁盘与体积控制

一个企业系统 5 个页面，源码 + 图片大概 20–80 MB。**MHTML 每份 2–10 MB**（含 base64 图片），4 个页面就是 8–40 MB。

**必须实现：**
- `plan.mirror.max_total_mb`（默认 300）：超过后**停止抓图片**，JS/CSS/HTML 继续抓（那些才是关键）
- 抓取前预估：`getResourceTree` 返回的 `resources` 里有 mimeType，按类型估算
- 全程累计字节，每抓一个更新；超限写一条 `warn` 事件并在 INDEX.json 记 `truncated: true`
- **优先级顺序**（超限时按此舍弃）：图片 > 字体 > CSS > MHTML > JS > HTML

### 3.6 单元测试（≥5 个）

1. `url_to_path` 处理 `..`、Windows 保留名、中文、query
2. `INDEX.json` 排序稳定（同样输入两次，字节级相同）
3. 超限时按优先级舍弃（构造 4 种类型的资源，验证先丢图片）
4. 失败资源出现在数组里且带 `skipped`
5. `sha256` 可复算

---

## 4. 模块二：操作快照带（`src/filmstrip.rs`）

### 4.1 目标

> 每次操作，前后各一张图。

现有：29 次点击 → 6 张图。**要做到 29 次点击 → 58 张图**（+完整 DOM）。

### 4.2 触发点

在 `app.rs::on_binding` 里，`"click"` / `"change"` / `"enter"` 三个分支：

```
收到事件
  ├─ 立刻：截"前"图 + 存完整 DOM      ← 必须在事件发生的同一 tick
  ├─ 等 400ms（可配）让 DOM 稳定
  └─ 截"后"图 + 存完整 DOM
```

**注意时序陷阱：**

1. **"前"图必须在事件处理的第一时间截**，不能等 `await` 其他事——但现在 `on_binding` 已经 `emit` 了一条事件，`emit` 会 `sync_data`（可能要几十毫秒）。所以顺序是：**先截图，再 `emit`**。
2. **"后"图用 `tokio::spawn` 异步延迟**，不能 `sleep` 阻塞事件循环（现有设计是单任务 250ms tick）。
3. 用 `SeqGuard` 保证同一个 `seq` 的 before/after 不会和其他事件交错。

### 4.3 文件名与对齐

```
filmstrip/
├── 000042-before.png
├── 000042-after.png
├── 000042-before.html      ← 完整 DOM（不截断）
├── 000042-after.html
└── …
```

`000042` 就是 timeline 行的 `seq`，**六位零填充，字典序 = 时间序**。

**必须能通过 `seq` 从 timeline 反查**：
```json
{"kind":"click","seq":42,"element":{...},"filmstrip":{"before":"filmstrip/000042-before.png","after":"filmstrip/000042-after.png"}}
```

### 4.4 完整 DOM 的要求（关键修正）

现有 `snapshot_dom` 的问题：`truncate_bytes(2MB)` 会**从中间切断 HTML**。

**新实现：**

```rust
// 不做字节截断，做"元素级"限制
const MAX_ELEMENTS: usize = 20000;
let html = cdp.evaluate(sid,
    "(function(){var n=document.querySelectorAll('*').length; \
      return document.documentElement.outerHTML;})()").await?;
```

- **不截断**（企业系统单页 DOM 通常 100 KB–1 MB，全存得起）
- 若真超过 5 MB，在**标签边界**截断：找最后一个完整的 `</...>` 位置切，并在末尾加 `<!-- TRUNCATED -->` 注释，保证结果仍是合法 HTML
- **不脱敏**（和 `mirror/` 同策略），因为要看真实控件和值
- 若页面在 iframe 里：主文档 + 各 iframe 分别存 `000042-before.html`、`000042-before-frame1.html`

### 4.5 截图裁剪（可选，但建议）

每次全页截图（`captureBeyondViewport: true`）在处理长列表页时可能是 2000×8000 的巨图。

**建议：**
- 默认**视口大小**截图（`captureBeyondViewport: false`），足够看"发生了什么"
- `--full-page-shots` 开启整页
- 若有多显示器 / 高 DPI，先 `Page.setDeviceMetricsOverride` 固定到 `1920x1080, deviceScaleFactor: 1`，**避免同一操作在不同机器上截出不同尺寸**（对后续比对很重要）

### 4.6 体积

29 次操作 × 2 张 × 约 150 KB = **约 9 MB**。500 单的真实跑批会是几千次操作——但探针只跑 3–5 个单号，可接受。

**保护：**
- `plan.filmstrip.max_total_mb`（默认 500）
- 超限后停止截图，**继续存 DOM**（DOM 更小且信息更密）
- `--no-filmstrip` 完全关闭

### 4.7 单元测试（≥3 个）

1. 文件名零填充正确（seq=42 → `000042`）
2. 截断只在标签边界发生，结果是合法 HTML（用一个会被字节截断的中文长文档测）
3. 超限后停止截图但继续存 DOM

---

## 5. 模块三：数据入口（`src/inputs.rs`）

### 5.1 三个命令

```
probe.exe attach --out <DIR> --port 9222 \
  --numbers <FILE>          # 单号清单
  --values <FILE>           # 固定值（手机号、投递员、模板名）
  --annotate                # 开启语义标注（模块四）
```

### 5.2 `--numbers <FILE>`

**支持格式（按扩展名）：**
- `.txt` / `.csv` / `.xlsx` 的**单列**——但 `xlsx` 需要 `calamine`，**这是新依赖**。

> **决策：本次只支持 `.txt` 和 `.csv`。**
> 理由：`calamine` 会拉进 `zip` + `quick-xml` + `encoding_rs` 等一串依赖，vendor 体积和审计成本都高。
> 让操作员在 Excel 里"另存为 CSV"是一次性的、可教会的事。
> 如果确实必须直接读 xlsx，**单独提一个二期需求**，不要偷偷加。

**解析规则：**
- 一行一个单号，`trim()` 后非空
- 去掉首尾引号、BOM、中文全角空格
- 校验：11–16 位纯数字（与 `redact.rs` 现有规则一致）；不合规的行**跳过并警告**，不中断
- 去重（保留首次出现顺序）
- 上限 2000 行，超出截断并警告

**产物：** 启动时写 `inputs.json`：

```json
{
  "source": "numbers.txt",
  "total": 5,
  "valid": 5,
  "numbers": [
    {"idx": 0, "masked": "7100*********161", "hash8": "a3f5c1d2", "used": false},
    ...
  ]
}
```

**`used` 字段由探针在识别到输入框里出现该单号时置 true**（见 §5.4）。

### 5.3 `--values <FILE>`

JSON 或 `key=value` 的 txt：

```json
{
  "receiver_phone": "13000000000",
  "delivery_person": "张三",
  "print_template": "国内快包面单-正式"
}
```

用途：
- **不自动填写**（探针不干预业务，这是铁律）
- 只在**识别输入框的值**时用来比对：操作员填了手机号，探针确认"填的是配置里那个"，记一条 `input_confirmed`
- 打印到控制台提示："本次固定值：手机号 130******00，投递员 张*"（脱敏显示）

**若配置文件里的值与操作员实际填的不一致** → 记 `warn`："手机号与配置不符：期望 130******00，实际 138******12"（只显示脱敏值）。

### 5.4 输入框值识别（补现有缺口）

**现状：** `record_click.js` 的 `change` 事件只发 `value_len` 和 `value_preview`（30 字符）。**不够**——单号被截断成 13 位看起来一样。

**改成：**

```js
// assets/record_click.js 的 change 分支
document.addEventListener('change', function (e) {
  var el = e.target;
  if (!el) return;
  var v = (el.value || '');
  send('change', {
    element: describe(el),
    value_len: v.length,
    // 新增：值指纹。原始值永不离开页面。
    value_hash: quickHash(v),          // 简单 FNV-1a，够用且无依赖
    value_masked: maskInPage(v),       // 前4位 + 星号，与 Rust 侧规则一致
    // 新增：像不像单号
    looks_like_mailno: /^\d{11,16}$/.test(v.trim()),
    // 新增：拆开后的单号数组（逗号/换行/空格分隔都能识别）
    tokens: v.split(/[\s,，;；]+/).filter(function (t) { return /^\d{11,16}$/.test(t); }).slice(0, 50)
  });
}, true);
```

**`maskInPage` 必须和 Rust 的 `redact::mask_number` 完全一致**（保留前 4 位，其余 `*`），否则两边对不上。**写一个测试，用同一组输入同时跑 JS 和 Rust，结果必须相同。**

**Rust 侧处理 `change` 事件时：**
1. 若 `looks_like_mailno` 或 `tokens` 非空 → 对每个 token 算 `sha256` 前 8 位，在 `inputs.json` 的 `numbers` 里查出对应项 → 置 `used = true`，记一条 `input_number` 事件：
   ```json
   {"kind":"input_number","seq":N,"hash8":"a3f5c1d2","masked":"7100*********161","field":"#mailNo","step":"A"}
   ```
2. 若 `value_hash` 与 `--values` 里某个值的 hash 相同 → 记 `input_confirmed`（用了配置里的固定值）
3. 若值长度 > 0 但不在已知清单里 → 记 `input_unknown`（操作员填了意料之外的东西，值得人工看）

**这解决了"单号→操作的关联"**：不再靠事后从请求体反推，而是**输入框里一出现就记下**。

### 5.5 剩余单号提示（可选，但很有用）

控制台每 30 秒打一行：

```
[单号] 已使用 3/5：7100*********161 ✓  7100*********204 ✓  7100*********317 ✓
       未使用 2：7100*********428 · 7100*********539
```

操作员能一眼看到还有哪几个没跑。

### 5.6 单元测试（≥4 个）

1. txt 解析：空行、BOM、全角空格、重复、非法值
2. `mask_number` 与 JS `maskInPage` 一致（用固定向量）
3. `hash8` 稳定
4. `used` 置位逻辑

---

## 6. 模块四：操作语义标注（`src/annotate.rs`）

### 6.1 为什么

现有只有 A/B/C/D 四个阶段，粒度太粗。

SOP 里有 20 来个具体动作（"输入邮件号"、"点确定"、"点详情单补打"、"勾全选"、"选模板"、"点打印"、"填手机号"、"点退回寄件人"……）。**报告里说"阶段B 有 12 次点击"，你根本不知道是哪 12 次。**

### 6.2 两种模式

**模式 A：热键标注（推荐）**

SOP 的每个动作分配一个编号 1–20，操作员每做一个动作按一次数字键（小键盘也行）。

```rust
// hotkeys.rs 扩展：注册 VK_NUMPAD0..9 和 VK_0..9
// 按数字 → 记录 "下一个操作是 SOP 动作 #N"
```

**实现要点：**
- 数字键是**单次有效**的（"下一个操作"），不是开关——避免操作员忘关
- 在控制台回显：`[标注] 下一个操作 = #7 勾选全选`（用 SOP 动作表的中文名）
- 若 5 秒内没有发生操作 → 自动失效并提示"标注 #7 已过期"
- **热键冲突是大概率事件**（同 §9 偏差 #8）：必须有 stdin 兜底，输入 `7` 回车

**模式 B：事后标注（兜底）**

若热键全部不可用，操作员事后在 `annotations.csv` 里填：

```csv
seq,动作编号,备注
42,5,点详情单补打
43,6,勾选全选
```

`probe annotate --out <DIR> --file annotations.csv` 把它合并进 timeline（作为 `annotation` 事件）。

### 6.3 SOP 动作表（`plan.json` 可改）

```json
{
  "sop_actions": [
    {"no": 1,  "step": "A", "name": "输入邮件号", "hint": "在'邮件号码'框里输入"},
    {"no": 2,  "step": "A", "name": "点确定",     "hint": "每个单号都要点一次"},
    {"no": 3,  "step": "A", "name": "点确认下段", "hint": "全部输完后"},
    {"no": 4,  "step": "B", "name": "点详情单补打"},
    {"no": 5,  "step": "B", "name": "粘贴单号串", "hint": "英文逗号，无空格"},
    {"no": 6,  "step": "B", "name": "点查询"},
    {"no": 7,  "step": "B", "name": "勾选全选"},
    {"no": 8,  "step": "B", "name": "选打印模板"},
    {"no": 9,  "step": "B", "name": "点打印"},
    {"no": 10, "step": "B", "name": "点确定打印"},
    {"no": 11, "step": "B", "name": "关闭弹框(x)"},
    {"no": 12, "step": "B", "name": "填收件人手机号"},
    {"no": 13, "step": "C", "name": "点退回寄件人"},
    {"no": 14, "step": "C", "name": "逐单输入+Enter"},
    {"no": 15, "step": "C", "name": "点妥投确认"},
    {"no": 16, "step": "C", "name": "点关闭"},
    {"no": 17, "step": "D", "name": "打开投递归班交接"},
    {"no": 18, "step": "D", "name": "选投递员"},
    {"no": 19, "step": "D", "name": "点查询"},
    {"no": 20, "step": "D", "name": "点确定"},
    {"no": 21, "step": "D", "name": "点 ->"}
  ]
}
```

> 这张表是**初稿**，DeepSeek 不要改内容。它来自 SOP，允许用户后续在 `plan.json` 里增删。

### 6.4 事件

```json
{"kind":"annotation","seq":N,"action_no":7,"action_name":"勾选全选","step":"B","mode":"hotkey"}
```

**关联规则：** `annotation` 事件之后、下一条 `annotation` 之前的所有 `click` / `change` / `enter` 事件，都归到这个动作下。在报告里按动作聚合。

### 6.5 报告里新增一节

```markdown
## 操作序列时间线

| # | 动作 | 阶段 | 时刻 | 点击次数 | 请求数 | 截图 |
|---|---|---|---|---|---|---|
| 1 | 输入邮件号 | A | 14:02:11 | 1 | 0 | 0042 |
| 2 | 点确定 | A | 14:02:12 | 1 | 1 | 0043 |
| ...
```

**这一节是给 DeepSeek 写自动化脚本看的最重要的一节**——它告诉你"这一步该点哪个元素、发什么请求、界面变成什么样"。

---

## 6A. 模块五：AGENT PACK —— 给 agent 吃的语料包

### 6A.1 先读这一段，它决定前四个模块怎么做

最终使用者不是人，是**一个 AI agent**。场景是：

> 包带回开发机 → 连同 SOP 原文一起丢给 agent → agent 直接写出可运行的自动化脚本。

这带来两条**凌驾于前四个模块之上**的原则：

**原则一：抓取要笨，分析要晚。**

如果 `ACTIONS.json`、`API.md` 这些摘要是探针在内网机上生成的，那么摘要逻辑写错，你只能再进一次内网。
所以**探针只负责尽可能完整、尽可能原始地抓**；所有解释、归纳、schema 推断，都放在开发机上跑的离线命令里。

**这条原则反过来约束前四个模块**：凡是"需要判断"的地方，一律只记录、不结论。
例如 §4 的 DOM diff，探针只存 before/after 两份原始 HTML，**diff 在离线阶段算**。

**原则二：agent 的上下文有预算。**

它读不了 80 MB。必须分层：

| 层 | 体积目标 | 内容 |
|---|---|---|
| L1 必读 | < 300 KB | 索引 + 动作表 + 定位器表 + 接口摘要 + 缺失清单 |
| L2 按需 | 每份 < 200 KB | 精简后的页面 HTML |
| L3 检索 | 不限 | 原始源码、原始 timeline、原始截图 |

`00-AGENT-READ-ME.md` 必须用一句话说明这三层的用法，并且**明确禁止 agent 一次性吞 L3**。

### 6A.2 命令

```
probe.exe agentpack --out <原始 out 目录> --dest <AGENT_PACK 目录> [--pages A,B,C,D]
```

**这条命令在开发机上跑，不在内网机上跑。** 它只读 `<out>`，不连浏览器。

输入依赖（都由前四个模块产出）：
`timeline.jsonl`、`filmstrip/`、`mirror/`、`inputs.json`、`annotations.csv`、`network.har`、`elements.json`

### 6A.3 产物结构

```
AGENT_PACK/
├── 00-AGENT-READ-ME.md          ← L1，索引与阅读顺序
├── ACTIONS.json                 ← L1，核心：动作→定位器→请求→界面变化
├── SELECTORS.json               ← L1，去重定位器表 + 唯一性证明
├── API.md                       ← L1，接口清单与调用契约
├── MISSING.md                   ← L1，没抓到什么（防幻觉）
├── PAGES/
│   ├── A-下段扫描.html           ← L2，精简 HTML，每份 < 200 KB
│   ├── B-详情单补打.html
│   ├── C-退回寄件人.html
│   └── D-投递归班.html
├── SCREENSHOTS/                 ← L3，按 seq 命名，与 ACTIONS 对齐
│   ├── 000042-before.png
│   └── 000042-after.png
└── RAW/                         ← L3，原样拷贝
    ├── mirror/
    ├── filmstrip/
    ├── timeline.jsonl
    ├── network.har
    └── ...
```

**所有路径用相对路径**（包里不能出现 `D:\probe\out` 这类本机路径，否则 agent 会照抄）。

### 6A.4 `ACTIONS.json` —— 最重要的一个文件

agent 写脚本时，90% 的时间在看这个文件。

```json
{
  "pack_version": 1,
  "generated_from": {"session_id": "20261117-143022", "rule_version": "0.1.0"},
  "actions": [
    {
      "no": 9,
      "name": "点打印",
      "step": "B",
      "seq": 42,
      "at": "2026-11-17T14:03:47+08:00",
      "selector": {
        "primary": "#printBtn",
        "backup": ["button.el-button--primary:nth-child(3)", "//span[text()='打印']/ancestor::button[1]"],
        "unique": true,
        "note": null
      },
      "pre_screenshot": "SCREENSHOTS/000042-before.png",
      "post_screenshot": "SCREENSHOTS/000042-after.png",
      "ui_delta": {
        "added": [
          {"tag": "div", "cls": "el-dialog__wrapper", "text": "请填写收件人手机号", "selector": "body > div:nth-child(15)"}
        ],
        "removed": [],
        "navigated": false,
        "url_after": "http://.../print/index"
      },
      "requests": [
        {
          "request_id": "1234.5",
          "method": "POST",
          "path": "/api/print/apply",
          "status": 200,
          "req_preview": "{\"mailNos\":[...],\"template\":\"国内快包面单-正式\"}",
          "resp_preview": "{\"code\":0,\"msg\":\"成功\"}",
          "har_entry": 27
        }
      ],
      "next_waits_for": {"type": "dom_modal", "selector": "div.el-dialog__wrapper", "timeout_ms_suggested": 5000},
      "annotation": {"action_no": 9, "mode": "hotkey"},
      "evidence": {
        "timeline_seqs": [42, 43],
        "raw_dom_before": "RAW/filmstrip/000042-before.html",
        "raw_dom_after": "RAW/filmstrip/000042-after.html"
      }
    }
  ],
  "unresolved": [
    {"seq": 57, "reason": "click 没有关联到任何请求，且 DOM 无变化，无法判断作用"}
  ]
}
```

**逐字段要求：**

- `next_waits_for` **必须给**。这是 agent 写 `waitForSelector` 的唯一依据，没有它生成的脚本必然脆弱。
  推断规则（离线算）：`ui_delta.added` 非空 → 等第一个新增的稳定元素；否则若 `requests` 非空 → 等该请求完成；否则标 `{"type":"none"}` 并在 `unresolved` 里记一笔。
- `ui_delta` 由**离线 diff** 两份原始 HTML 得出，探针不参与判断。diff 前先做规范化：去空白、去内联 style 随机值、按 tag+id+class+text 建指纹。
- `unresolved` 里的条目**不许猜**。agent 看到 `unresolved` 就知道这一段需要人工确认。
- 一个动作可能有多条 `requests`（点击触发多个并发请求），全部列出，**不要只取第一条**。
- `annotation.action_no` 缺失时用自增编号，并在 `MISSING.md` 里说明"本次没有语义标注，动作名是推断的"。

### 6A.5 `SELECTORS.json`

按"控件"聚合，而不是按"点击"聚合——同一个按钮点 12 次，只出现一条。

```json
{
  "selectors": [
    {
      "key": "B.打印按钮",
      "primary": "#printBtn",
      "backup": ["..."],
      "unique": true,
      "occurrences": 12,
      "first_seq": 42,
      "stability": "high",
      "why": "id 稳定，class 含 css-1x2y3z（CSS-in-JS，不可靠，故不作主定位器）"
    },
    {
      "key": "D.-> 按钮",
      "primary": null,
      "backup": ["button:nth-child(4)"],
      "unique": false,
      "occurrences": 1,
      "stability": "low",
      "why": "无 id/name/text，同级有 4 个相同结构的按钮，需按位置或图标区分"
    }
  ]
}
```

`stability` 判定（离线算）：`id` 命中 → high；`name`/`data-*` → medium；仅 CSS path → low。
**`unique:false` 的必须出现在 `MISSING.md` 的风险段**。

### 6A.6 `API.md`

从 HAR + mirror 反推，给 agent 判断"能不能走协议回放"。

```markdown
## POST /api/print/apply

- 触发动作：#9 点打印
- 请求头：`Content-Type: application/json`、`X-Token: <会话内固定，见 HAR entry 27>`
- 请求体（实测样本）：
  ```json
  {"mailNos": ["710000000004161"], "template": "国内快包面单-正式", "ts": 1794897871136}
  ```
- **动态字段：`ts`（毫秒时间戳，每次不同）。其他字段在 3 次样本中恒定。**
- 响应体：
  ```json
  {"code": 0, "msg": "成功", "data": {"failed": []}}
  ```
- 错误语义：`code != 0` 时为业务失败，`msg` 为中文原因（样本：无）
- 签名：**未检测到**（无 sign/nonce 字段，header 无变化）
- 结论：**可直接协议回放**，只需每次刷新 `ts`

---

## POST /api/scan

- 触发动作：#2 点确定
- **动态字段：无**
- 结论：可直接协议回放
```

**判定规则（离线算）：**
- 字段名匹配 `^(ts|time|timestamp|nonce|sign|signature|token|_t|rand)$` 或值在多次样本中不同且长度 > 8 → 标为动态
- 若 body/header 出现疑似密文（高熵字符串、base64 长串）→ 标"**疑似加密，需读源码确认**"，并给出来源文件路径和搜索建议
- 每个接口必给一句结论：`可回放` / `需复现签名` / `不可回放`

### 6A.7 `MISSING.md` —— 防幻觉

**这是最容易被省掉、但最该保留的一份。**

```markdown
# 本次采集的缺口

> 以下内容**没有采到**。不要推断，不要假设它们在真实系统里不存在。

## 完全缺失
- 阶段 C 的响应体：`/api/deliver/confirm` 有请求但 body 被系统回收，`Network.getResponseBody` 返回 4042
- 打印预览窗口的截图：当时 `PrintWindow` 超时（3s）

## 部分缺失
- `mirror/` 缺 6 个图片资源（超过 512 KB，按策略跳过）
- `timeline.jsonl` 有 3 条 request 未关联到 click（可能由定时器触发）

## 置信度低
- `D.-> 按钮` 的定位器不唯一（同级 4 个同结构按钮），实机需人工确认
- 阶段 B 的"分支 B2（弹出一串框）"未复现，无法确认触发条件

## 建议补采
1. 重新做一次 C 段，让 `/api/deliver/confirm` 的响应体落盘
2. 在 D 段点击 `->` 前先截图，确认按钮的可视特征
```

**生成规则：** 凡是 `ACTIONS.json` 的 `unresolved`、`SELECTORS.json` 的 `unique:false`、`INDEX.json` 的 `skipped`、`timeline` 里 `body_status != "ok"` 的，全部汇总进来。**自动化收集，不要人工写。**

### 6A.8 `00-AGENT-READ-ME.md` —— 索引

```markdown
# 这个包是什么

内网系统「内网寄递业务平台」退货流程的一次完整操作记录。
你拿到的目标是：**写出能自动完成 SOP 四个阶段的可运行脚本。**

# 阅读顺序（严格按此，不要跳）

1. `MISSING.md` —— **先读**。知道什么没有，才不会编。
2. `ACTIONS.json` —— 21 个动作的完整链路。这是你写代码的主要依据。
3. `SELECTORS.json` —— 控件定位器。`unique:false` 的一定要看 `why`。
4. `API.md` —— 判断哪一步可以走接口、哪一步必须点界面。
5. `PAGES/*.html` —— 需要看页面结构时读，每份 < 200KB。

# 不要做的事

- **不要**一次性读 `RAW/` 下的所有文件（会撑爆上下文）。按需检索。
- **不要**假设 `MISSING.md` 里列出的东西存在。
- **不要**用截图推断元素 id——id 在 `SELECTORS.json` 里。

# 目录说明

| 路径 | 用途 |
|---|---|
| `ACTIONS.json` | 动作 → 定位器 → 请求 → 界面变化 → 等待条件 |
| `SELECTORS.json` | 去重后的定位器与稳定性评级 |
| `API.md` | 接口契约与可回放性判定 |
| `PAGES/` | 精简 HTML（已剥离脚本与样式，保留结构与文本） |
| `SCREENSHOTS/` | 按 seq 命名，与 ACTIONS 的 pre/post 对应 |
| `RAW/` | 原始素材，只在需要核对时检索 |

# 数据说明

采集于 <日期>，共 <N> 次操作、<M> 个请求。
SOP 原文见随包附带的 Word 文档。
```

### 6A.9 `PAGES/` 精简规则（L2 层）

从 `mirror/` 的原始 HTML 提取：

1. 删除所有 `<script>`、`<style>`、`<link rel=stylesheet>`
2. 删除所有内联 `style="..."`（保留 `display:none` 这类有语义的）
3. 删除注释、`data-v-*` 之类的构建产物属性
4. 压缩空白，但**保留缩进层级**（agent 要靠层级理解结构）
5. **保留**：`id`、`name`、`class`、`type`、`placeholder`、`value`（脱敏后）、`role`、文本内容
6. 目标每份 < 200 KB；超出时按"只保留表单控件及其祖先链"裁剪，并在文件头加注释说明

### 6A.10 验收

- [ ] 把 `AGENT_PACK/` 连同 SOP 原文交给一个 agent，**不给任何额外说明**，它能产出覆盖 21 个动作的脚本骨架
- [ ] 脚本里引用的每个 selector 都能在 `SELECTORS.json` 里找到
- [ ] 每个动作都有 `next_waits_for`
- [ ] `MISSING.md` 里的条目与 `unresolved` / `unique:false` / `skipped` 一一对应，无遗漏
- [ ] L1 五个文件合计 < 300 KB
- [ ] `PAGES/` 每份 < 200 KB
- [ ] 包内无绝对路径（`grep -r "D:\\\\" AGENT_PACK/` 为空）
- [ ] `agentpack` 能在**没有内网、没有浏览器**的机器上跑完

### 6A.11 单元测试（≥4 个）

1. DOM diff 能识别"新增一个 dialog 节点"，不把随机 style 值算成变化
2. 动态字段识别：`ts`/`nonce` 命中，`template` 不命中
3. `MISSING.md` 汇总覆盖全部四类来源
4. 包内路径全为相对路径

---

## 7. 集成点（精确）

| 位置 | 改动 |
|---|---|
| `src/lib.rs` | `pub mod mirror; pub mod filmstrip; pub mod inputs; pub mod annotate;` |
| `Cargo.toml` | **不加任何依赖**（`Debugger`/`getResourceTree`/`captureSnapshot` 都是现有 `cdp.call` 直发） |
| `src/cdp/commands.rs` | 加 `get_resource_tree`、`get_resource_content`、`capture_snapshot_mhtml`、`enable_debugger`、`get_script_source` 五个薄封装 |
| `src/app.rs` `struct App` | 加 `mirror: Mirror`、`film: Filmstrip`、`inputs: Inputs`、`annotator: Annotator` |
| `src/app.rs` `on_binding` | **调换顺序：先 filmstrip.before()，再 emit**；`change` 分支加 §5.4 的识别；`click`/`enter` 后 `spawn` 延迟截图 |
| `src/app.rs` 事件分派 | 加 `Debugger.scriptParsed` 分支；`Network.loadingFinished` 里对非 XHR 资源补取 body |
| `src/app.rs` `finish` | 写 `INDEX.json`、`inputs.json`、`annotations.csv`、mirror 收尾 |
| `src/netcap.rs` | `loadingFinished` 后的 body 采样规则从"只 XHR/Fetch"扩到"Script/Stylesheet/Image/Document"，按 §3.5 优先级和大小限制 |
| `src/hotkeys.rs` | 加数字键标注（模式 A）+ 5 秒过期 |
| `src/app.rs` `spawn_stdin_control` | 加数字 1-21、`MIRROR`（打印镜像进度）、`#`（看 SOP 动作表） |
| `src/cli.rs` | `--numbers` `--values` `--annotate` `--no-filmstrip` `--full-page-shots` `--no-debugger`；新增 `Cmd::Annotate`、`Cmd::MirrorScan`、`Cmd::Agentpack` |
| `src/agentpack/mod.rs` | 新增：离线编译器主流程 |
| `src/agentpack/actions.rs` | 新增：动作聚合 + DOM diff + `next_waits_for` 推断 |
| `src/agentpack/selectors.rs` | 新增：定位器去重 + 稳定性评级 |
| `src/agentpack/api.rs` | 新增：HAR → `API.md`，动态字段识别 |
| `src/agentpack/pages.rs` | 新增：HTML 精简（L2） |
| `src/agentpack/missing.rs` | 新增：缺口汇总 |
| `src/report/markdown.rs` | 新增「操作序列时间线」一节（§6.5）和「源码镜像」一节 |
| `operator-card.md` | 全面重写（见 §10） |

**`main.rs` 的 `panic_out` 和 `dispatch` 两个 `match` 都要加新分支**，漏一个就编译失败。

---

## 8. 禁令

1. ❌ **不要为了体积牺牲完整性。** 先抓全，再谈压缩。
2. ❌ **不要脱敏 `mirror/` 里的源码。** 脱敏会破坏 JS 语法。
3. ❌ **不要自动填写任何表单。** 探针只记录。
4. ❌ **不要 auto-accept JS 弹窗以外的任何确认框**（现有 `--dialog-policy accept` 保持）。
5. ❌ **不要截断 HTML 到非法状态。** 必须标签边界。
6. ❌ **不要引入 `calamine` 或任何 xlsx 解析库**（见 §5.2）。
7. ❌ **不要让 `Debugger.enable` 默认开启后无法关闭。**
8. ❌ **不要 `sleep` 阻塞事件循环**（延迟截图必须 `spawn`）。
9. ❌ **不要跳过 `Page.getResourceTree` 的失败分支**——失败要记 `skipped`，不能静默。
10. ❌ **不要回退 README §9 的任何一条偏差。**

---

## 9. 验收

### 9.1 回归

- [ ] `cargo test` **≥ 43 passed / 0 failed**（31 原有 + ≥12 新增）
- [ ] `cargo build --release --offline` 成功
- [ ] 原有 12/12 H2、12/12 H1、10/10 续跑 **全部仍通过**

### 9.2 镜像（模块一）

- [ ] mock 页跑完 A→B→C→D 后，`mirror/` 下能找回 mock 的 `app.js` 和 `style.css`
- [ ] `INDEX.json` 里 `total == captured + skipped`
- [ ] 在浏览器里用 `file://` 打开 `mirror/.../index.html`，**页面骨架能显示**（截图存证）
- [ ] 每个页面有至少 1 份 `.mhtml`，用浏览器能打开还原成截图
- [ ] 构造一个动态 `eval` 的脚本，能在 `mirror/inline_scripts/` 里找到
- [ ] `--no-debugger` 时跳过运行时脚本，其余照常
- [ ] 超过 `max_total_mb` 时先丢图片（构造大图测）

### 9.3 filmstrip（模块二）

- [ ] mock 上 20 次点击 → `filmstrip/` 里 **≥ 40 张 png**
- [ ] 任取一个 `seq`，`000042-before.html` 是**合法 HTML**（能被解析器完整解析）
- [ ] timeline 的 `click` 事件里有 `filmstrip` 字段，路径真实存在
- [ ] `--no-filmstrip` 时一张都不截，其余功能不受影响
- [ ] `--full-page-shots` 时图片高度 > 视口高度

### 9.4 数据入口（模块三）

- [ ] `--numbers` 喂 5 个单号，操作员输入其中 2 个后，`inputs.json` 里这 2 个 `used: true`
- [ ] `change` 事件的 `tokens` 能识别 `单号1,单号2,单号3` 这样粘贴的多单号
- [ ] JS `maskInPage` 与 Rust `mask_number` 输出一致（固定向量测试）
- [ ] 非法单号被跳过且警告，不中断
- [ ] 配置里的手机号与操作员实填不一致时记 `warn`

### 9.5 标注（模块四）

- [ ] 按数字键 7 后做点击，`annotation` 事件正确记录
- [ ] 5 秒无操作后标注过期
- [ ] 热键不可用时，stdin 输入 `7` 回车等效
- [ ] `report.md` 的「操作序列时间线」有 21 行（或实际操作数），每行有截图引用

### 9.6 交付

- [ ] 一份 mock 产出的完整 `out` 目录（可打 zip 发给评审）
- [ ] 更新后的 `operator-card.md`
- [ ] `README.md` 新增「镜像与快照」「数据入口」两节，并更新 §9 偏差表
- [ ] 一句话说明：**这次改动后，拿到包的人能不能在离线环境里把页面打开**

---

## 10. 操作员卡（重写要点）

新流程比原来长，必须写清楚：

```
准备（技术做）：
  把 numbers.txt（一行一个单号）和 values.json 放到 D:\probe\

第 1 步  双击 启动Edge.bat
第 2 步  在打开的浏览器里登录，把 4 个业务页面都打开一遍
         （每个页面停留 3 秒，让探针把源码抓下来）
第 3 步  技术启动探针，看到 [覆盖] 行说明连上了
第 4 步  照常操作。每做一个动作前，先按对应数字键：
         1 输入邮件号   2 点确定   3 点确认下段
         4 点详情单补打  5 粘贴单号   6 点查询   7 勾全选
         8 选打印模板   9 点打印    10 点确定打印  11 关闭弹框
         12 填手机号
         13 点退回寄件人 14 逐单输入+Enter 15 点妥投确认 16 点关闭
         17 打开投递归班 18 选投递员 19 点查询 20 点确定 21 点 ->
         忘了按也没关系，能按就按，按了结果更准
第 5 步  做 3-5 个单号就够
第 6 步  输入 END 回车结束
第 7 步  打包 D:\probe\out 整个目录
```

---

## 11. 实施顺序

| # | 内容 | 验收 |
|---|---|---|
| 1 | `cdp/commands.rs` 五个新封装 | 编译通过，mock 上能取到 `/api/version` |
| 2 | `mirror.rs` 资源树 + 正文 + INDEX.json | §9.2 前四条 |
| 3 | `mirror.rs` MHTML + 运行时脚本 | §9.2 五、六条 |
| 4 | `mirror.rs` 体积控制 + `mirror-scan` | §9.2 七条 |
| 5 | `filmstrip.rs` 截图 + 完整 DOM | §9.3 全条 |
| 6 | `inputs.rs` 单号清单 + 值识别 | §9.4 全条 |
| 7 | `annotate.rs` 热键 + stdin + 事后合并 | §9.5 全条 |
| 8 | `app.rs` 集成：顺序调整、事件分派、finish | 端到端跑通 |
| 9 | `agentpack/` 动作聚合 + DOM diff + `ACTIONS.json` | `next_waits_for` 全部有值或进 `unresolved` |
| 10 | `agentpack/` 定位器去重 + `API.md` + `MISSING.md` | 三份文件能互相对应 |
| 11 | `agentpack/` `PAGES/` 精简 + `00-AGENT-READ-ME.md` | L1 < 300 KB，每页 < 200 KB |
| 12 | **让一个 agent 只拿 `AGENT_PACK/` 试写脚本** | §6A.10 全条 |
| 13 | `report/markdown.rs` 两节新增 | 报告可读 |
| 14 | `operator-card.md` 重写 | 给不懂技术的人看能看懂 |
| 15 | 重打 `release/` + `cargo vendor` | 干净 Win11 `--offline` 构建 |

**第 12 步是整个项目的真验收。** 如果 agent 拿着一份没有额外说明的 `AGENT_PACK/` 写不出脚本，前面 11 步全白做。这一条不要用"应该可以"结案，要真的跑一次。

---

## 12. 最后一句

判断这次做没做成，只看一件事：

> **拿到包的人，能不能在一台没有内网的机器上，把那个页面用 `file://` 打开、把 `filmstrip/` 翻一遍，然后把自动化脚本写出来，不需要再进一次内网。**

做不到，就再做一轮。
