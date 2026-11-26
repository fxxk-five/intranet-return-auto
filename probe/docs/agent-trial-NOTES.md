# NOTES —— 写脚本时不确定、需要人工确认的点

交付物：`OUT/automation.py`（Python + Playwright 同步 API 骨架）、本文件。
未改动 `D:\AGENT_PACK` 下任何既有文件；产物只落在 `OUT/`（运行时另生成 `OUT/run-artifacts/`）。

复核命令（不需要浏览器）：

```bash
cd D:\AGENT_PACK\OUT
python automation.py --selfcheck    # 脚本 ↔ SELECTORS.json / ACTIONS.json 逐条对账
python automation.py --dry-run      # 打印解析出的 seq、定位器、每步等待
```

---

## 1. 定位器合规性：一个必须说明的例外

脚本里所有**控件**定位器都来自 `SELECTORS.json` 的 `primary`/`backup`，代码中每个 `Sel(...)` 与
`Spec(...)` 都带 `key=` 注释（16 个 key 全部用到，`grep 'key="' automation.py` 可核对）。

例外是两个**等待目标** `#modalMask` / `#modalBody`：它们出现在 `ACTIONS.json` 的
`next_waits_for.selector` 里，但 `SELECTORS.json` 只登记“可点击/可输入控件”，没有这两条。
处理方式：

- 不硬编码方向，改为 `WAIT_TARGETS` 两个常量，各自标注来源（`next_waits_for`）；
- `verify_pack()` 启动时校验：这两个 selector 必须真的出现在包的 `next_waits_for` 里，否则报错退出；
- 因此它们不是“脚本自创的 selector”，而是包给定的等待目标。

**需要人工确认**：若评审要求“每个 selector 都必须在 SELECTORS.json 有条目”，需把
`#modalMask`、`#modalBody` 补进 SELECTORS.json（`stability` 建议 high，`unique: true`），
脚本改两行即可；否则这条要求与 `ACTIONS.json` 的等待条件本身冲突。

---

## 2. 为什么 seq 没有写死（很重要）

任务期间包被**反复重新生成**（不是我看错：`ACTIONS.json` / `MISSING.md` 的 mtime 从 19:23 起至少变了四次，每次 seq 与等待类型都可能变）：

| 时间 | 变化 |
|---|---|
| 19:23 我第一次读 | A 阶段 ×:31、确定:35、change:42；B ×:84、打印:91；C #btnReturn:103；`111` 与 `51` 的等待是 element |
| 19:26 再读 | 变成 ×:32、36、43、×:85、打印:90、#btnReturn:102；`51` 变 request |
| 19:29 再读 | 又变成 ×:31、40、44、×:84、打印:90、102；`51`/`111` 都是 request；`C.退回寄件人` 的 primary 从 `button.tab` 变成 `#btnReturn` |
| 19:32 再读 | seq 与 19:29 相同，仅 `MISSING.md` 文案变化（本文件引用的就是这一版） |

结论：把 33 个 seq 写成常量，任何一次重采集都会让脚本静默错位。所以现在的结构是：

1. `SPECS` 只按 **SOP 顺序** 声明“做什么”（step / kind / SELECTORS key / prefer）；
2. `build_actions()` 在 import 时按“阶段内顺序”与 `ACTIONS.json` 对齐，取回真实的
   `seq / no / name / next_waits_for`，并检查 kind 是否一致；
3. 若包里动作数量多于声明（脚本漏步）或 kind 不符，直接抛错，不静默少跑；
4. `verify_pack()` 再做一遍 key 集合、primary/backup/unique/stability、prefer 合法性对账。

**需要人工确认**：这种“按阶段内顺序对齐”的假设是——同一阶段的动作顺序稳定。
如果真实平台上的 SOP 顺序会变（例如有时先查询后全选），必须改 `SPECS` 顺序；
顺序变化不会自动被发现（只能靠 kind 计数不符时报错）。

---

## 3. 三类等待的实现口径（我自己定的，供评审）

| `next_waits_for.type` | 实现 | 理由 |
|---|---|---|
| `element`（=dom） | `expect(locator).to_be_visible()/to_be_hidden()`，超时用包给的 `timeout_ms_suggested`（缺省 5000） | 同一个 `#modalMask` 在“打开”后可见、在“关闭”后带 `hidden` 类，`wait_for_selector(state=…)` 只能表达单向；`expect` 两个方向都能断言，超时信息还能带出实际状态 |
| `request` | `page.expect_response(谓词)` 包住整段动作；谓词 = 方法一致 + URL 以 `path` 结尾（容忍 `?query`）；随后校验 `status==200 && body.code==0` | 包里的 9 个 POST 都返回 `{code,msg,count,serverTime}`，`code!=0` 应视为失败；用后缀匹配可避免把静态资源算进来 |
| `none` | 固定静默窗口 `--none-settle-ms`（默认 400ms），**不冒充验证** | 记录里确实没有可判定条件（`MISSING.md` 也把它们列为“意图不明”）。本来可用 `#status` 的“请求中/完成”文本当条件，但 `#status` 没有登记在 SELECTORS.json，按约束不敢用 |

补充：元素等待的**方向**（visible / hidden）包里的 `next_waits_for` 不写，`ui_delta` 又自相矛盾，
所以方向由动作语义决定，规则只有三条：

- 目标是 `#modalBody`（只在“关一条、看下一条”时出现）→ 等可见；
- 动作是关闭按钮 `#modalClose` → 等隐藏；
- 其余（确定 / 确认下段 / 妥投确认 / 打印等结果类）→ 等可见。

`--selfcheck` 会把“脚本方向 ≠ ui_delta”的动作全部列出来（当前 7 条，见下一节）。

---

## 4. 包里记录互相矛盾的地方（每一处都要人工确认）

### 4.1 A 阶段 `ui_delta` 的可见性前后颠倒

同一份 `ACTIONS.json` 里：点“确定”后记成 `#modalMask … hidden`，点“×”后记成
`#modalMask … visible`（每轮都一样，不是偶发）。而页面源码 `RAW/mirror/app.js:119` 与
`:79` 明确是“确定成功 → showModal”“× → closeModal”。两者只能有一个对，我按源码语义写，
并在 `--selfcheck` 里把这 7 条列出来：

| seq（当前包） | ui_delta 记录 | 脚本 | 依据 |
|---|---|---|---|
| 27 / 36 / 44 | hidden | visible | app.js:119 确定成功后弹窗 |
| 31 / 40 / 49 | visible | hidden | app.js:79 × 关闭 |
| 88 | visible | hidden | 队列最后一次 ×，实际由 `drain_modal_queue()` 按状态自适应 |

**需要确认**：真实平台点击“确定”后是否真有弹窗、是否有动画延迟；若没有弹窗，
这些 `element` 等待要改成 `request` 或 `none`，否则会 5s 超时。

### 4.2 B 阶段“打印”的分支行为

`ACTIONS.json` 只记到 4 次打印点击 + 6 次 ×，语义靠 `MISSING.md` 是看不出来的。行为来自
`RAW/mirror/app.js:162-196`：第 1 次 → 原生 `alert`；第 2 次 → 无弹窗；第 3 次 → **一次压入
5 条** DOM 提示；第 4 次起 → 每次弹“填写收件人手机号”（最多 3 轮，且每轮要再点一次打印）。

因此记录里的 **6 次 × 与源码的 5 条队列不一致**。脚本没有硬编码次数，而是
`drain_modal_queue()`：逐次 ×，每关一条后看实际状态——下一条出现就等
`#modalBody` 可见，遮罩消失就等 `#modalMask` 隐藏，队列空了就提前结束并把记录值写进日志。

**需要确认**：真实平台打印到底是“三次点击三个分支”还是别的形态；这个分支逻辑几乎肯定
是演示站特有的，真实平台更可能是“每次打印 → 一个确认框”。若如此，SPECS 里 B 阶段的
顺序需要重排（打印与 × 交替），不能照抄录制。

### 4.3 单号输入被记成 `change`（动作名“点确定”）

A 阶段 3 次输入 `#mailNo` 在包里是 `kind: change`，第一条的名字却叫“点确定”
（`annotation.action_no=2`，热键标注），而 `#mailNo` 本身不触发任何请求。
脚本按“填值”处理（`fill` 会派发 `input`+`change`），弹窗/请求由随后的“确定”兜住。

**需要确认**：真实平台是否需要在输入后按 `Enter` 或失焦才触发校验（演示站是点“确定”才发请求）。

### 4.4 C 阶段 `#btnReturn` 的 key 归属

录制里 `seq=102/107` 的 `primary` 是 `#btnReturn`，但 `SELECTORS.json` 没有独立的
`#btnReturn` 条目——它只作为 `key="C.退回寄件人"` 的 `backup` 出现（这个 key 的 primary 是页签
`button.tab`）。脚本用 `key="C.退回寄件人" + prefer="#btnReturn"` 表达，
`verify_pack()` 校验“记录 primary ∈ 该 key 的 primary/backup”。**建议补一条独立 SELECTORS 条目**，
否则这个映射会一直靠约定维持。

### 4.5 `#status` 文案与 `RAW/assertions.json` 的结论冲突

`assertions.json` 里 `A_scan_confirm` 记为“未观测到 A 阶段弹窗（pass=false）”，
但 `ACTIONS.json` 明确记了 `#modalBody: 确认下段完成，共 3 个单号`，`PAGES/*.html` 里也有这段逻辑。
我按 `ACTIONS.json` + 源码写断言（`assert_modal_contains("确认下段完成")`）。
**需要确认**：以哪份为准；若真实平台确认下段不弹窗，这条断言要降级为只校验请求。

---

## 5. 包里没有、脚本里留 TODO 的东西

| 缺口 | 影响哪一步 | 脚本怎么处理 | 建议 |
|---|---|---|---|
| `#printNos`（B 阶段补打单号输入框）不在 `SELECTORS.json` | B 阶段“查询”前 | `gap("print_nos")` → TODO 日志，跳过填值（`/api/print-query` 会带空数组） | 实机确认定位器后补进 SELECTORS.json，并在 `phase_b` 加 `fill` |
| `#phoneInput`（B 阶段手机号弹窗输入框）不在 `SELECTORS.json` | B 阶段最后一步 | `gap("phone_input")` → 直接点“确定”，`/api/print-phone` 带空 `phone` | 同上；真实平台大概率校验手机号，空值会失败 |
| `#returnMailNo`（C 阶段退回单号输入框）不在 `SELECTORS.json`，且录制里**没有对应的输入动作** | C 阶段退回 | `gap("return_mail_no")` 后照录制点 `#btnReturn`；实际会因请求没发出而超时，错误信息已指向此缺口（**这是刻意设计的失败**，不是 bug） | 必须先补定位器 + 明确“退回哪几个单号” |
| `#status`（底部状态栏）未登记 | 所有 `none` 等待 | 不用它做条件，改用固定静默 | 若要更稳，把它补进 SELECTORS.json 后用“状态不再是『请求中』”做条件 |
| `#panel-D`（“未开始/已交接”）未登记 | D 阶段结束态 | `gap("panel_d")` → 只靠 `/api/handover` 响应证明完成 | 补定位器后可加文本断言 |
| 登录 / 鉴权 | 全部 | 未实现，脚本假定浏览器已有会话 | 需要 cookie/token 注入方式或登录步骤录制 |
| iframe | 全部 | 假定表单在顶层文档 | 若真实平台用 iframe，`resolve()` 需要改成 `frame_locator` |
| 真实内网地址 | 全部 | 无默认值可用（`RAW/mirror` 目录名 `127.0.0.1_8080` 是演示站） | 用 `--base-url` 传入 |
| 单号 | A/C | 包里只有脱敏占位 `7100***********`；必须 `--numbers-file` 提供 | — |

补充：`MISSING.md` 本次说“响应体全部拿到了”，所以脚本对响应体做 `code==0` 校验是安全的；
但 `MISSING.md` **没有**提到上面这些控件定位器缺失——这是 `SELECTORS.json` 与 `PAGES/*.html`
之间的落差，属于包本身的盲点，我已在此记录。

---

## 6. `unique:false` 的 5 条怎么定位的

`B.详情单补打`、`B.确定`、`C.关闭`、`C.退回寄件人`、`D.投递归班交接` 都是“没有 id，靠 class”。

- 候选顺序：`prefer`（如果有）→ 该 key 的**文本 XPath** backup → `primary` → 其余 backup；
- 每个候选取**第一个可见**的匹配（`first_visible`），因此 4 个页签、两个“确定”（面板里的
  `#btnOk` 与弹窗页脚的“确定”）都能按可见性区分开；
- `C.退回寄件人` 同时代表页签和 `#btnReturn`：页签用 `prefer="//button[…='退回寄件人']"`
  （DOM 里 nav 在 panel 之前，第一个可见即页签），面板按钮用 `prefer="#btnReturn"`。

**需要人工确认**：

1. 真实平台的 class 是否带构建哈希（包里已警告）；若带，`button.tab`/`button.btn` 会失配，
   需改成 `[data-tab]`/文本定位并补进 SELECTORS.json。
2. 弹窗页脚的“确定/关闭”在真实平台若有 id，务必补条目替换文本定位。
3. 文本定位对多语言/空格敏感（用了 `normalize-space()`），换肤后要复核。

---

## 7. 我做到的验证 / 没做到的验证

**做了**

- `python -m py_compile`：语法通过。
- `python automation.py --selfcheck`：16 个 key、33 个动作、2 个等待目标全部与包对齐；
  输出同时列出 7 条“元素等待方向与 ui_delta 相反”的动作供人工复核。
- 逻辑层冒烟测试（临时脚本，未放进交付物）：用按 `RAW/mirror/app.js` 行为写的**假页面**
  跑 A/B/D 三个阶段——A 发出 3×`/api/scan` + 1×`/api/scan-confirm` 且弹窗断言通过；
  B 走完 4 次打印（alert 被 accept、5 条提示被逐条关闭）、`/api/print-phone` 发出；
  D 发出 `/api/handover`；C 如预期在“缺 `#returnMailNo` 填值”处失败，错误信息指向该缺口。

**没做**

- **没有在真实浏览器里跑过**：本机 `python -c "import playwright"` → `ModuleNotFoundError`。
  需要 `pip install playwright && playwright install chromium` 后，用
  `--base-url http://<内网地址> --numbers-file nos.txt --headed` 实机验证。
- 因此以下都属“纸面推断”：真实平台的弹窗动画时序、`#modalMask` 是否常驻 DOM、
  打印是否真会调起浏览器打印对话框（`RAW/network.har` 里 **没有** 打印相关请求，
  说明打印是纯前端行为，但真实平台未必）。

---

## 8. 哪些步骤可以走接口（`API.md` 的结论与我的取舍）

`API.md` 判定 9 个 POST 全部“可直接协议回放”，只有 `/api/handover` 需要每次重新生成 `ts`。
按这个结论，A/C/D 都能纯接口完成；B 阶段则**必须点界面**：`/api/print-query`、
`/api/print-phone` 是接口，但真正的“打印”在包里没有任何请求（`network.har` 只有 10 条 API +
3 条静态资源），`#btnConfirmPrint`（确定打印）甚至没被录制过。

我仍然把整条链路写成界面驱动，原因：

1. 壳子要先证明“界面路径能走通”，接口化是之后的有意优化；
2. `count` 类字段的语义依赖页面状态（`/api/delivered` 的 `count` 是 `#returnList li` 条数，
   `RAW/mirror/app.js:214`），脱开界面就得自己维护这份状态；
3. 打印/手机号弹窗是纯前端交互，接口化绕不过界面。

**建议**：实机确认后，A/C/D 改成“拿 session cookie → `page.request.post` 直接打接口”，
只保留 B 走界面；届时 `count` 需要由脚本显式维护（**已确认** `scan-confirm` 传已扫描数、
`delivered` 传已退回条数，但真实平台是否也这样算，需要抓一次包确认）。

---

## 9. 其他需要人工确认的小点

1. **单号轮次**：A 阶段录制只覆盖 3 个单号（`00-AGENT-READ-ME.md` 说“3/5”）。脚本按
   录制的三元组循环；超过 3 个时复用最后一组模板（无录制佐证，已打警告）。
2. **`alert` / `confirm`**：`RAW/dialogs.json` 记了 1 个 `dom_modal` + 2 个 `js_dialog`
   （打印第 1 次的 alert、妥投确认前的 confirm），但 `ACTIONS.json` 没把它们记成动作。
   脚本统一 `accept`（`attach_dialog_policy`）。若真实平台用 `window.confirm` 做二次确认、
   而业务上应当取消，这里要改成按文案分支处理。
3. **B 阶段手机号轮次**：源码里最多 3 轮（`phoneLoop < 3`），录制只走了 1 轮。脚本只做 1 轮。
4. **页签切换方式**：`PAGES/*.html` 里页签带 `data-tab="A|B|C|D"`，但 `SELECTORS.json` 没登记
   这个属性，所以脚本按文本点击。`data-tab` 明显更稳，**建议补进 SELECTORS.json 后替换**。
5. **`A.点确定` 与 `A.7100***********` 是同一个元素**（都是 `#mailNo`），SELECTORS.json 因
   录制动作名不同而保留了 2 条 key；脚本按 SPECS 分别引用，语义上无差别。
6. **`A.×` 的 `backup` 里有 XPath `//input[normalize-space()='7100***********']`**（`A.点确定`/
   `A.7100***********` 的 backup 同样）：这条 XPath 匹配的是 `input` 的文本内容而不是 `value`，
   对输入框实际取不到值，属无效备选。脚本把它排在 `#mailNo` 之后，所以不影响运行，
   但**建议从 SELECTORS.json 删掉或改成 `//input[@value='…']`**。
7. **提示文案断言**：`assert_modal_contains` 依赖“下段扫描成功”“确认下段完成”“妥投确认完成”
   这些中文串，换文案就会失败——它们来自 `RAW/mirror/app.js` 与 `ACTIONS.json.ui_delta.text`。
