# 退货流程取证探针（probe）

内网寄递业务平台内网取证探针。**只观测、不干预业务**：它把操作员的手工流程变成
可分析的证据（请求、控件、弹窗、打印行为、耗时），最终产出一份能回答 Q1–Q7 的
`report.md`。

设计规格见上级目录的 `探针设计方案-Rust-v1.md`；本文件只讲**怎么编译、怎么部署、怎么跑**。

---

## 1. 交付物

```
probe/
├── src/                 Rust 源码
├── mock/                本地模拟页（无内网环境自测用，不是业务代码）
├── vendor/              依赖离线副本（只在需要重新编译时用，266 MB）
├── .cargo/config.toml.offline   离线编译配置（需改名为 config.toml 才生效，见 §2.3）
├── release/             ★ 直接可用的交付物（probe.exe + 启动脚本 + 操作卡）
├── operator-card.md     操作员操作卡
└── README.md            本文件
```

内网机上只需要 `release/` 目录。

---

## 2. 编译（在**有网**的机器上做，产物是单个 exe）

```bash
cargo build --release --offline
# 产物：target/release/probe.exe（约 2.4 MB，无需 VC++ 运行库）
```

### 2.1 三个会卡住你的环境坑（都是实测踩过的）

1. **构建路径必须全是 ASCII。**
   `x86_64-pc-windows-gnu` 的链接器是 GNU ld/dlltool，它们用系统 ANSI 代码页接收
   命令行参数。源码/工具链/target 目录里有中文（例如 `D:\code\内网退货自动处理程序`）
   时，ld 会报 `cannot find ...rlib: No such file or directory`。
   → 把 `CARGO_TARGET_DIR` 指到纯英文路径（如 `D:\probe-target`），工具链也放在英文路径。

2. **必须有 `dlltool.exe`。** `windows` crate 的 `windows_x86_64_gnu` 构建脚本要调它。
   MSVC 工具链没装时可以只装 mingw-w64（本工程用的是 winlibs 的 msvcrt 版），
   把它加进 `PATH` 即可。

3. **Rust 工具链的 `PATH` 要用 Windows 形式**（`D:\rt\rust\bin;...`）。
   用 MSYS 的 `/d/rt/...` 形式时，cargo 子进程（build script、rustc）用 `CreateProcess`
   找不到 `rustc`/`dlltool`，报 `program not found`。

本机开发脚本 `../dev.sh` 已经把这些都设好了：

```bash
bash dev.sh build            # debug
bash dev.sh build --release  # release
bash dev.sh test             # 单元测试
```

### 2.2 离线构建（vendor/）

`vendor/` 已包含全部依赖。但 **GNU ld 处理不了非 ASCII 路径**（见 2.1 第 1 点），
所以离线构建要在**纯英文路径**下做：

```bat
copy .cargo\config.toml.offline .cargo\config.toml
cargo build --release --offline
```

当前工程目录含中文（`内网退货自动处理程序`），所以这里的 `config.toml` 默认是关掉的
（改名成 `.offline`），否则本机连 `cargo test` 都链不起来。

### 2.3 换一台干净机器（有 MSVC 的情况）

装了 Visual Studio Build Tools 的话，把 `Cargo.toml` 的 `[dependencies.windows]` 换成
默认 MSVC 目标即可，`rustup default stable-x86_64-pc-windows-msvc` 后
`cargo build --release --offline` 直接可用（`-gnu` 与 `-msvc` 的差别只在链接方式）。

---

## 3. 内网机部署（3 步）

1. 把 `release/` 整个目录拷到内网机，例如 `D:\probe\`。
2. 双击 `启动Edge.bat`：它用**独立 profile** 拉起 Edge 并开好调试端口。
   - 独立 profile 是必须的：本机已有 Edge 在跑时，新进程会把调试端口"让"给已有实例，9222 永远不监听。
   - 弹出的 Edge 窗口里登录平台，开好 4 个业务标签页。
   - ⚠️ 这个窗口**不要关掉重开**，重开会断开探针。
3. 技术侧在黑窗口里跑：

```bat
probe.exe preflight --out D:\probe\out          :: 全 [OK] 再继续
probe.exe launch  --out D:\probe\out            :: 如果 preflight 用法不同见下
```

更稳的做法是分两步（浏览器由操作员自己启动时用 attach）：

```bat
probe.exe preflight --out D:\probe\out
probe.exe attach    --out D:\probe\out
```

> `launch` 自己拉起浏览器，`attach` 挂到已经带 `--remote-debugging-port=9222` 启动的浏览器。
> 现场最常用的是 **launch**（省得操作员记参数）。

跑完后把 `D:\probe\out` 整个目录打包回传（含账号信息，走内网传输）。

---

## 4. 操作员做什么

见 `operator-card.md`。一句话：**照平时的顺序操作，每换一个阶段按一次热键**，
按 `Ctrl+Alt+F9` 结束。热键不可用时在探针窗口输入 `A`/`B`/`C`/`D`/`END` 回车。

---

## 5. 产物说明（`out/sessions/<时间戳>/`）

| 文件 | 内容 |
|---|---|
| `report.md` | 人读的一页纸结论，十二节，对应 Q1–Q7 |
| `report.json` | 同样内容的机读版 |
| `network.har` | 标准 HAR 1.2，可直接拖进 Edge DevTools 看请求 |
| `elements.json` | 全部可交互元素 + 定位器（含唯一性校验与控制语义映射） |
| `dialogs.json` | 四类弹窗分类表（DOM 模态 / JS 弹窗 / 原生窗口 / 打印预览） |
| `experiment21.json` | Q2 判定（H1/H2/H3）与依据 |
| `assertions.json` | 流程里程碑断言 |
| `timeline.jsonl` | 全事件流（点击、请求、弹窗、截图、原生窗口），可续跑 |
| `screenshots/` | 页面截图 + 原生窗口截图 |
| `snapshots/` | 分阶段 DOM 快照（已脱敏） |
| `replay/scenario.json` | 去噪后的最小请求序列（协议回放模板） |
| `secrets/secrets.json` | ⚠️ 完整凭据/单号，**不脱敏**，禁止外传 |
| `hotkeys.json` | 本次实际生效的热键表 |

`report.md` 首行自带警告；除 `secrets/` 之外，所有文本都做过脱敏（单号保留前 4 位、
手机号保留前 3 后 2、凭据整值替换）。

---

## 6. 设计上刻意没做的事

- **不用 `chromiumoxide`**：CDP 协议本身极简单（`{id,method,params}` 收 / `{id,result}` 回），
  自己写 400 行换来完整的原始事件流。
- **不给 Edge 加 `--disable-print-preview` / `--kiosk-printing`**：
  前者会跳过"打印预览窗口"——那正是 Q5 要判定的对象，等于把要观测的现象关掉再观测；
  后者会真的出纸。打印链路的证据改用**被动观测 spooler**（只读 `EnumJobs`）。
  后续正式程序走 CDP `Page.printToPDF`（不经过 spooler、不弹窗），不用这两个参数。
- **不自动点击业务按钮**：探针只注入只读取证脚本。唯一会发真实请求的是
  `probe.exe experiment`，它有 5 道闸（见下）。
- **不用 reqwest**：它拉 rustls→ring，ring 需要 C 工具链，违反"不引入需要 C 编译器的 crate"。
  探针只需要对本机 `127.0.0.1` 发明文 HTTP/1.1 GET，`src/http1.rs` 自己写。

---

## 7. `experiment` 的保护措施（唯一会发真实请求的命令）

```bat
probe.exe experiment --out D:\probe\out --request-id <id> --numbers 21,22,30 ^
        --test-number 0000000000001 --confirm-dry-run
```

1. 缺 `--confirm-dry-run` → 退出码 2，打印告警；
2. 缺 `--test-number` → 退出码 2（禁止代真实待处理单号）；
3. 启动后 10 秒倒计时，可 Ctrl+C；
4. 只发 `--numbers` 里的 N 个变体，串行、间隔 3 秒；
5. 每个响应记状态/预览/耗时/是否业务错误，最后给对比表与 `q2_verdict`。

---

## 8. 自测（开发机）

```bash
python mock/serve.py 8080          # 模拟页（开发期用，不进交付物）
python ../tools/e2e.py default      # 期望 q2=H2
python ../tools/e2e.py h1           # 期望 q2=H1
python ../tools/e2e_resume.py       # 断点续跑
```

`tools/` 里的脚本是**测试脚手架**，不是交付物；它们会用 CDP 模拟"操作员点击"，
探针本身绝不这么做。

---

## 9. 影印机：镜像 / 快照 / 数据入口 / 语义标注

探针不再只是"诊断器"，它把整个系统搬到磁盘上，让拿到包的人能在**没有内网**的机器上
写完自动化脚本。四个模块：

| 模块 | 产物 | 一句话 |
|---|---|---|
| 源码镜像 | `mirror/` | 全站 HTML/JS/CSS/图片 + MHTML + 运行时脚本，`INDEX.json` 记来源与 sha256 |
| 操作快照带 | `filmstrip/` | 每次点击/输入**前后各一张图 + 完整 DOM**，按 `seq` 命名 |
| 数据入口 | `inputs.json` | 单号清单、固定值、以及"操作员实际输入的是清单里第几个" |
| 语义标注 | `annotations.csv` | 每个动作对应 SOP 第几步 |

### 9.1 源码镜像

- `mirror/<host>/...` 保持原 URL 路径；`mirror/mhtml/*.mhtml` 是自包含存档；
  `mirror/inline_scripts/` 存没有 URL 的 `eval`/动态脚本。
- **`mirror/` 不脱敏**（脱敏会破坏 JS 语法），因此 `00-先读我.txt` 里写了外传禁令；
  要复核是否含凭据：`probe.exe mirror-scan --out <out>` → `mirror/FINDINGS.md`。
- 体积上限 `--mirror-max-mb`（默认 300）。超限时**先丢图片**，再字体，再 CSS，再 MHTML；
  JS/HTML 永远保留（那些才是复现的关键）。
- 验收：`mirror/<host>/index.html` 用浏览器 `file://` 打开即可看到页面骨架（样式和布局都在）。

### 9.2 操作快照带

- `000042-before.png` / `000042-after.png` / `000042-before.html` / `000042-after.html`，
  `000042` 就是 timeline 的 `seq`，六位零填充，**字典序 = 时间序**。
- 时序是硬要求：**先截图，再写 timeline**（写记录会 `sync_data`，几十毫秒，晚截就拍不到点击前状态）；
  "后"图用 `spawn` 延迟 400ms，且**限并发 2**（并发几十个截图会把渲染进程压死）。
- 完整 DOM 不做字节截断；超过 5MB 才在**标签边界**切，并加 `<!-- TRUNCATED -->`。
- `--no-filmstrip` 关闭；`--full-page-shots` 整页（默认只截视口）。
- 体积上限 `--filmstrip-max-mb`（默认 500）：超限后**停止截图但继续存 DOM**。

### 9.3 数据入口

```bat
probe.exe launch --out D:\probe\out --numbers numbers.txt --values values.json --annotate
```

- `--numbers`：一行一个单号（txt/csv），校验 11–16 位纯数字，去重、跳非法并警告，上限 2000 行。
  **不自动填写**——只做识别：操作员输入时探针记下"这是清单里第几个"（`input_number` 事件）。
- `--values`：JSON 或 `key=value`，用于核对操作员填的手机号/模板是不是配置里那个。
  两边只交换 FNV 指纹，原始值不离开页面。
- 控制台每 30 秒打印一行"已使用 3/5"，操作员随时知道还剩哪几个单号。

### 9.4 语义标注

- 每做一个动作前按 `Ctrl+Alt+1..9`（或黑窗口输入数字回车，两位数用这个），
  标记"下一个操作是 SOP 第 N 步"，5 秒内有效、单次生效。
- **为什么不用裸数字键**：`RegisterHotKey` 会全局吃掉按键，操作员就没法在"邮件号码"框里输数字了。
- 事后补标注也支持：编辑 `annotations.csv`（`seq,动作编号,备注`）后跑
  `probe.exe annotate --out <out> --file annotations.csv`，会合并进 timeline。

---

## 10. AGENT_PACK（离线编译给 agent 吃的语料包）

```bat
probe.exe agentpack --out D:\probe\out --dest D:\AGENT_PACK
```

**只在开发机上跑，只读 `<out>`，不连浏览器。**

产物：

```
AGENT_PACK/
├── 00-AGENT-READ-ME.md   L1 索引与阅读顺序（先读 MISSING.md）
├── ACTIONS.json          L1 动作 → 定位器 → 请求 → 界面变化 → 等待条件
├── SELECTORS.json        L1 去重定位器 + 稳定性
├── API.md                L1 接口契约与可回放性判定
├── MISSING.md            L1 缺口清单（防幻觉）
├── PAGES/*.html          L2 精简页面（每份 < 200KB）
├── SCREENSHOTS/          L3 与 ACTIONS 的 pre/post 对应的截图
└── RAW/                  L3 原始素材（mirror / filmstrip / timeline / har），**不含 secrets/**
```

设计原则：**抓取要笨，分析要晚**。内网只负责抓原始数据；所有归纳（DOM diff、
`next_waits_for` 推断、动态字段识别、定位器评级）都在开发机上算——这里改错重跑不要钱，
内网改错要再进一次内网。

`ACTIONS.json` 里每条动作都有 `next_waits_for`；推不出来就进 `unresolved`，**不许猜**。

## 11. 实现与方案书的偏差（逐条都可追溯）

方案书是依据，不是教条。下面每一处偏离都有理由，改回去之前请先看理由。

| # | 方案书写法 | 实际实现 | 为什么 |
|---|---|---|---|
| 1 | 单实例锁用 `CreateFileW` + `FILE_SHARE_READ` | 命名互斥体 `Local\probe_single_instance`，PID 仍写 `.lock` 供排障 | 同样是内核对象级独占，但不依赖共享模式语义，也不会被安全软件的文件扫描误判 |
| 2 | 邮件号"13 位" | 11–16 位连续数字都按单号脱敏（保留前 4 位） | 方案书自己的例子 `710000000004161` 就是 15 位；现场单号位数不唯一。宁可多剥一层 |
| 3 | 单号计数用正则数 13 位数字 | JSON body 里只在"键名像单号"的字段里数；非 JSON 才用正则 | `{"ts":1794897871136}` 的毫秒时间戳会被数成单号，把 Q2 判据带偏（自测真的踩到了） |
| 4 | Q2 看"确定按钮关联的请求" | 再加一层 URL 排除（print/phone/deliver/return/handover/query/confirm） | 真实系统里"确定"到处都是，打印/退回/交班都叫确定；不排除会把无关请求算进 H2/H3 |
| 5 | 原生窗口里 class 含 `Chrome_WidgetWin` 判 `print_preview` | 只有标题含"打印/Print"或 class 是 `PrintPreview/PrintDialog` 才判 | 浏览器每个窗口都是 `Chrome_WidgetWin`，按原规则会把浏览器自己的窗口全算成打印预览，Q5/Q7 直接失真 |
| 6 | `--disable-print-preview` / `--kiosk-printing`"仅在确认分支行为后启用" | **完全不实现**，并在禁令里写明 | 不止"可能出纸"：前者会跳过打印预览窗口，而那正是 Q5 要判定的对象——等于把要观测的现象关掉再观测。打印证据改用被动读 spooler |
| 7 | `EnumWindows` / `PrintWindow` 同步调用 | 放到 `spawn_blocking` + 3s 超时；spooler 查询 5s 超时并降频到 30s | 实测：现场默认打印机是网络打印机时，`OpenPrinter/EnumJobs` 能把整个事件循环冻住几十秒（自测时真的冻过一次）。`PrintWindow` 也可能卡在目标窗口 UI 线程上 |
| 8 | 热键 `Ctrl+Alt+F1..F4` | 配置表 + 三级整组降级 + `MOD_NOREPEAT` + `hotkeys.json`；另有 stdin 兜底（输入 `A/B/C/D/END` 回车） | 内网机上热键被占用是大概率事件，而这件事在开发机上测不出来。stdin 兜底让"热键全废"不致命 |
| 9 | 用 `reqwest` 取 `/json/version` | `src/http1.rs` 手写 60 行 HTTP/1.1 客户端 | `reqwest` 的 rustls 路径会拉 `ring`，`ring` 需要 C 工具链，违反"不引入需要 C 编译器的 crate" |
| 11 | 数字键 1–21 标注 | `Ctrl+Alt+1..9` + stdin 输入两位数 | `RegisterHotKey` 用裸数字会全局吃掉按键，操作员就没法在单号输入框里打数字了 |
| 12 | 每页一份 MHTML | 每个阶段一份 + 导航后补一份 | 阶段切换就是"换业务页"；导航后补一份才不会漏掉登录后的页面 |
| 13 | 每次操作全页截图 | 默认只截视口，`--full-page-shots` 才整页 | 长列表页整页截图可能 2000×8000，几十次就是几百 MB，而且对比时没有额外信息 |
| 14 | 非 XHR 正文"补取" | `Page.getResourceTree` + `getResourceContent` 为主，`getResponseBody` 为后备 | `getResourceContent` 不依赖 requestId，已缓存资源也能取到；两条路都留才有兜底 |
| 15 | 延迟截图直接 spawn | 用信号量限并发 2 | 实测：几十个截图同时打 CDP 会把渲染进程压到无响应，连弹窗都处理不了（表现为驱动侧超时） |
| 16 | 弹窗放行在主循环处理 | 在 CDP 传输层直接 `Page.handleJavaScriptDialog` | 主循环可能正卡在截图上，而页面被 alert 冻住时截图也不会返回——两边互等就是死锁 |
| 17 | 包内路径 | `agentpack` 自检绝对路径，命中即计入缺口 | 包里出现 `D:\...` 之类路径，agent 会照抄到脚本里 |
| 10 | `session` 每次启动新建目录 | 上次没写 `finished` 标记就**续用同一目录**，`seq` 从已有最大值继续 | 方案书同时要求"新目录"和"重启后 latest 指向同一 session"；只有按"未完成则续跑"才能两者兼顾 |

---

## 12. 一句话说明

**这次改动后，拿到包的人可以在没有内网的机器上把页面用 `file://` 打开、把
`filmstrip/` 从头翻一遍、照着 `ACTIONS.json` 把脚本写出来，不需要再进一次内网。**
（实测：mock 会话产出的包，`mirror/<host>/index.html` 用 `file://` 打开可以正常渲染；
`AGENT_PACK` 在无浏览器、无内网的条件下由 `agentpack` 一条命令生成。）

---

## 13. 第 12 步验收：让一个真 agent 只用 AGENT_PACK 写脚本

按 PROMPT §11 第 12 步做的实测：把 `D:\AGENT_PACK` 交给一个**没有任何额外说明**的 agent，
只告诉它"按包里的 00-AGENT-READ-ME.md 做事"。

**结果：成功了，但它列出的问题比脚本本身更值钱。**

产出（已归档在 `docs/`）：

| 文件 | 内容 |
|---|---|
| `docs/agent-trial-automation.py` | 约 975 行 Playwright 骨架，覆盖 ACTIONS.json 全部动作，**每个 selector 都标注了 SELECTORS.json 的 key** |
| `docs/agent-trial-NOTES.md` | 它自己发现的 6 类疑点 |

它做对的地方：
- 自动按 `MISSING.md → ACTIONS.json → SELECTORS.json → API.md → PAGES/` 的顺序读，没有翻 `RAW/`
- 每个动作都按 `next_waits_for` 写了等待（element / request / none 三种分别处理）
- 发现包里没采到的（`#printNos`、`#phoneInput`、C 阶段输入）**没有编**，留了 TODO 并让 `--strict` 直接失败
- 自己在启动时校验 selector 集合与 ACTIONS 一致，不一致就退出

它暴露的问题（下一轮要改进的，已进 README 而不是隐瞒）：

1. **seq 不可当作稳定 ID。** 我在它工作期间重生成过一次包，seq 整体重编号，它只能改成"运行时读 ACTIONS.json"。→ 下一版应给每个动作加一个跨会话稳定的 `action_uid`（阶段+动作名+出现次序），seq 只作本次会话内的对齐键。
2. **`ui_delta` 方向可能颠倒（7 处）。** 点击"确定"→弹窗出现、点击"×"→弹窗消失，但 before/after 的差分方向反了。原因：`bindingCalled` 在捕获阶段发出，页面自己的 handler 可能已经同步执行完，before 截图拍到的已经是"之后"。→ 下一版把 before 截图挪到 `Input.dispatchMouseEvent` 之前的 `Runtime.bindingCalled` 首行，或改用 `Page.domContentEventFired` 之外的更早钩子；短期靠 `--selfcheck` 人工复核。
3. **只有被"点击"过的控件才进 SELECTORS。** 纯输入框（`#printNos`、`#phoneInput`）没有点击事件，就没进表。→ 下一版把 `change` 的目标也并进 SELECTORS，并从 `elements.json` 的 control_map 补全。
4. 它没法验证真实浏览器行为：测试机没装 playwright。这部分只能等内网真机跑一趟。

**结论**：`AGENT_PACK` 这条链路是通的——agent 不看内网、不看源码树，只凭包就能写出骨架，
并且**能准确说出包里缺什么**。这正是"影印机"要的：缺口可见，而不是让下一个人猜。

---

## 14. 收尾轮（第 3 轮）：6 个缺陷 + SOP 一致性校验 + 引导模式

这一轮不是新功能轮，全部是**实测发现的缺陷**和一件一直被漏掉的事。

### 14.1 六个修复

| # | 缺陷 | 修法 | 验收证据 |
|---|---|---|---|
| 1 | 脱敏值漏进 selector（`//input[normalize-space()='7100***********']`） | 所有用到文本/值的候选先生 `looks_masked`（连续 2 个 `*`）；过滤掉的候选不生成，key 改用 `tag#id` | selector 字段里 `**` 计数 **0**；`element_ref` 补上"页面内第 N 个元素"的稳定引用 |
| 2 | `MISSING.md` 空节写成"响应体全部拿到了" | 空节一律只写 `无`，不写任何解释性结论；`未语义标注` 在全标注时整节省略 | 全文不含「拿到了」 |
| 3 | 纯输入框不进 `SELECTORS` | 聚合来源从 1 个扩到 3 个：click / change / `control_map`；输入框补 `input_kind`、`maxlength`、`observed_values` | `#printNos`、`#phoneInput` 进表，`sources` 含 `change` |
| 4 | `ui_delta` 方向颠倒 | ① `pointerdown` 时在页面内**同步**序列化整页 HTML（3MB 上限）② `Page.startScreencast` 环形缓冲 30 帧，取时间戳 <= pointerdown 的那帧作"前"图 ③ diff 改按 `tag+id/class` 身份比对，显隐切换算加/删 | 点"确定"→ `added` 含弹窗；点"×"→ `removed` 含弹窗、`added` 空；`next_waits_for.type=="none"` 占比从 **45% → 0%** |
| 5 | `seq` 被当稳定 ID | 新增 `action_uid = 阶段:动作名:出现次序`，推断名带 `~` 前缀并标 `name_from:"inferred"`；README 里明写"引用动作用 `action_uid`" | 同一份 out 连跑两次 `agentpack`，`action_uid` 集合完全相同 |
| 6 | 退化标签（`B.on`）进表 | 判退化：长度<2 / 状态词 / 纯数字 / 脱敏值；命中时 key 换成 `tag#id`。`describe()` 文本取值改为 `aria-label > title > placeholder > innerText` | `SELECTORS.json` 无 `on`/`off` key，且控件仍在表里（`B.#checkAllTop`） |

### 14.2 新增：SOP 一致性校验（`probe sopdiff`）

**这是这一轮真正的目标。** 原链路是「人照文档做 → 探针记录」；如果她本来就照做，
探针永远发现不了**文档**是错的。现在把文档变成期望序列去比对：

```bat
probe.exe sopdiff --out D:\probe\out --plan plan.json
```

- 期望表在 `plan.json` 的 `sop_flow`（21 步，含 `repeat.min/max` 与 `required`）
- 输出 `SOP-DIFF.md`：`missing` / `unexpected` / `over_repeat` / `under_repeat` / `order`（LCS 覆盖率 < 0.8）
- **标注率 < 50% 时文件开头强制写"置信度低"**——不许假装结论可靠
- 同时进 `AGENT_PACK` 的 L1（agent 先看到"文档可能有问题"）和 `report.md` 第二节

实测：把 `plan.json` 里"点确定"的 `max` 改成 1，再跑 `sopdiff` → 立刻报
`| 2 | 点确定 | 1-1 | 5 |`，结论从"未发现不一致"变成"25 处差异"。**配置真的生效。**

### 14.3 新增：引导模式（`--guide`）

一趟性任务的保险：黑窗口每步提示 `[引导] 下一步 #7 勾选全选（B 阶段）`，
点错当场提示 `[引导] 偏差：#7 期望「勾选全选」，实际点了「确定」`。

- 只打印，**不改变任何业务行为**
- `启动Edge.bat` 默认开启
- 操作员忘了按数字键也不会白跑：没标注的会按名字模糊匹配，并在 `SOP-DIFF.md` 里标低置信度

### 14.4 明确不做：自动登录

需求里提过"自主登录"。**本轮不实现**，理由写在 §10 里（一趟性任务登录失败就白跑；
内网普遍 SSO/验证码/UKey；密码进配置又多一个泄漏面）。实际做法是**登录态复用**：
独立 profile 持久化，**首次人工登录一次，之后所有运行都不再需要登录**。
`--auto-login` 只留接口位置，不实现。

### 14.5 本轮回归

| 项 | 结果 |
|---|---|
| 单元测试 | **80 passed / 0 failed**（收尾轮新增 13 个） |
| H2 端到端 | **27/27** |
| H1 端到端 | **27/27** |
| 断点续跑 | **10/10** |
| `--no-filmstrip` / `--no-debugger` | **5/5** |
| `none` 等待占比 | **0%**（要求 < 20%） |
| `action_uid` 稳定性 | 两次编译完全相同 |

---

## 15. 第二次 agent trial（收尾轮 §13.4）

拿到**只含新包**的独立会话，不给额外说明，让它按 `00-AGENT-READ-ME.md` 写脚本。

**结论：三项验收全过。**

| 验收项 | 结果 |
|---|---|
| 脚本里有含 `*` 的 selector | **0**（全文 5 处 `**` 都是 markdown 粗体和脱敏值展示，不是 selector） |
| 引用 `seq` 而不是 `action_uid` | **0**（37 个动作全部用 `action_uid`；`seq` 只在日志与排序里出现，脚本里还写了"禁止作为引用键"） |
| 对 `SOP-DIFF.md` 的 missing 项是否编造 | **没有**。10 条差异逐条给了处理方式（明确标注 ambiguous / 留 TODO / `NeedHumanDecision` 退出码 2） |

它自己还做了 selfcheck 和冒烟：`FAIL 0 / WARN 9`，对着包内 mock 起服务 + 按 `API.md` 打桩，
**37/37 步跑通**，观测到 10 个 POST 与 10 个带请求动作一一对应。

产出归档在 `docs/agent-trial2-automation.py`（811 行）与 `docs/agent-trial2-NOTES.md`（320 行）。

### 15.1 它暴露的 3 个产品问题（都已修）

1. **7 处等待自相矛盾**：`next_waits_for` 对"关闭"类动作写了"等这个按钮自己 visible"，
   可点完关闭它必然不可见 → 脚本第一次跑就卡死。**已修**：关闭/取消类动作改为 `state:"attached"`。
2. **标注被 `change` 吃掉**：`A:点确定:1` 的 kind 是 `change@#mailNo`——打字事件消费了"下一个动作"标记。
   **已修**：标注只由 `click`/`enter` 消费（真实系统里打字很频繁，这点在真机上会更明显）。
3. **动作名里带数据值**：`B:~710000000004140,7100:1`——输入框的值被当成了动作名。
   **已修**：名字是数据型（含逗号 / 6 位以上数字 / 含脱敏星号）时退回 `tag#id`。

修完重新出包：`names with comma: []`，关闭类 `state` 为 `attached`，标注落在 `确定` 这个 click 上。

### 15.2 它顺手指出的真实缺口（属于采集范围，不是 bug）

- D 阶段的"选投递员 / 查询 / 确定"**页面上根本没有这些控件**，SOP 可能写错或漏步骤 → 正是 `SOP-DIFF.md` 要回答的问题
- `#template` 模板选择、"确定打印"(`/api/print-confirm`) 本次没采到（驱动没做这两步）
- 手机号是循环，mock 里记录到 1 轮（真实系统会循环多次）
- 真实地址与登录态不在这份包里的（登录态在 profile 里，不落盘进 out）

**一句话**：agent 现在能对着包写出可跑的骨架，并且**能准确说出"哪一步它没底"**——
这比"看起来写完了"重要得多。

---

## 16. 第 4 轮补强：业务页正文与 iframe DOM（2026-11-20）

第一趟内网回传暴露了两个采集缺陷，已修：

1. **Document 响应体只存了前 2 KB。** 业务页面 HTML 只有 10–15 KB，本来完全存得起。
   现在 `Network.getResponseBody` 拿到的 Document 正文**全量**写到
   `sessions/<id>/pages/<host>/<path>.html`，timeline 的 response 事件里用 `body_file` 指回去，
   报告和离线分析可以直接读。
2. **业务表单在 iframe 里，之前只抓了门户外壳。**
   新增 `capture_full_dom`：在页面内递归遍历**同源 iframe（最多 3 层）**，
   每个文档单独落盘到 `pages/dom-<seq>[-frameN].html`；`mirror_harvest` 时抓一次、
   收尾时再补一次。`agentpack` 的 `PAGES/` 会优先从 `pages/` 里按阶段关键词
   （投递下段 / 打印补打 / 妥投寄件人 / 投递归班）挑对应页面。

验证：mock 里加了一个 iframe（标题「投递下段主界面」），
`tools/e2e_mirror.py` 现在 11/11，其中包含
「落了完整页面 DOM（含 iframe）」「iframe 里的业务页也被抓到」「Document 正文已落盘并记路径」。
原有回归全绿：85 单测、H2 27/27、H1 27/27、自动巡页 6/6、页面先开 6/6、续跑 10/10、开关 5/5。
