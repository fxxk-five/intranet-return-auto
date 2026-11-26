# NOTES —— 不确定 / 需人工确认的点

> 配套产物：`OUT/automation.py`（骨架）。本文只写“没把握”的部分，已确定的语料事实不重复。
> 引用一律用 `action_uid`（`seq` 只在日志里出现，不作引用键）。
> 证据来源缩写：`ACT`=ACTIONS.json、`SEL`=SELECTORS.json、`API`=API.md、`MISSING`/`SOPDIFF`=同包 md、
> `PG`=PAGES/*.html、`DLG`=RAW/dialogs.json、`ASR`=RAW/assertions.json、`EXP21`=RAW/experiment21.json、
> `RPT`=RAW/report.json、`MOCK`=RAW/mirror/127.0.0.1_8080/app.js（**注意：MOCK 是复刻用的模拟站源码，
> 它自称“刻意做成真实平台可能的样子”，所以只能当强线索，不能当真实平台的事实**）。

---

## 0. 一句话结论

37 个动作全部覆盖、全部按 `next_waits_for` 写了等待、定位器全部落在 `SELECTORS.json` 内；
但**这些动作是否等于“能自动跑完四个阶段”还不成立**，卡点在下面 4 类：

1. B 阶段（详情单补打）的打印/手机号是**循环**，记录里只采到 1 轮（§3.2）；
2. D 阶段（投递归班）SOP 有 3 个动作（选投递员/查询/确定）**完全没有采到**，页面精简版里也没有对应控件（§5.1）；
3. 3 个 Tab 与 2 个“确定/关闭”按钮**没有唯一 id**，运行期靠“可见 + 文本”收敛（§4）；
4. 包里所有单号/手机号都是脱敏的，真实地址也没记录（§6）。

---

## 1. 把握度分级（哪些动作没把握）

| 把握 | 动作（action_uid） | 为什么没把握 |
|---|---|---|
| 低 | `B:~关闭:1` … `B:~关闭:6` | 6 次“关闭”没有对应的 6 个 DOM 模态：MOCK 里第 3 次点打印只排队 5 个提示框，第 6 次关闭点的是已隐藏的 `#modalClose`。次数/对象都对不上（§3.2） |
| 低 | `B:~710000000004140,7100:1` | 它的 `next_waits_for` 是 `td`，但 MOCK 里 `td` 只有点“查询”后才渲染；输入完就等 `td` 很可能超时。记录可能把等待错位到了下一步（§2.2） |
| 低 | D 阶段 SOP 的 3 步 | 无动作、无定位器、无页面控件，无法实现，只能留 TODO（§5.1） |
| 中 | `A:点确定:1` | 名字是“点确定”（hotkey 标注），`kind` 却是 `change`、定位在 `#mailNo`；SOPDIFF 又说 A 阶段“输入邮件号”未记录。三种说法互相矛盾（§3.1） |
| 中 | `B:~详情单补打:1`、`C:~退回寄件人:1`、`D:~投递归班交接:1` | 都是 Tab，`primary` 同为不唯一的 `button.tab`（SEL：occurrences=5、stability=low、unique=false）。运行期按“可见+文本完全等于动作名”收敛，收敛不到就抛 `NeedHumanDecision` 停下（§4.1） |
| 中 | `B:~确定:1`、`C:~关闭:1` | `primary` 是 `button.btn`，页面上十来个同类按钮；靠“可见+文本=确定/关闭”收敛。B 页面里 `#btnConfirmPrint`（“确定打印”）是主要干扰项（§4.2） |
| 中 | `B:~on:1`、`B:~on:2` | 点的是 `<input type=checkbox id=checkAllTop>`；MOCK 只挂了 `click`，没有 `change`，但记录里 `B:~on:2` 是 `change` 事件，说明真机可能另有绑定。脚本对 `B:~on:2` 只做断言、不重复操作（§3.5） |
| 中 | `C:~妥投确认:1` | 真机上 `confirm()` 之后才发 `POST /api/delivered`；脚本默认点“是”。但**包内没有任何证据说明操作员点的是“是”还是“取消”**，只有“之后确实有 delivered 请求”这一条间接推断（§3.4） |
| 高 | 其余 24 个 | 唯一 id 定位 + 等待条件明确：`A:~确定:1..3`、`A:~关闭:1..4`、`A:~确认下段:1`、`A:~input#mailNo:1/2`、`B:~查询:1`、`B:~打印:1..4`、`B:~input#phoneInput:1`、`C:~input#returnMailNo:1/2`、`C:~退回寄件人:2/3`、`D:~→:1`。注意“高”只代表“按记录能复现”，不代表“真机能跑通”（见 §2、§6） |

---

## 2. 等待语义（`next_waits_for` 的落地方式）

### 2.1 三种非标准写法

- `state` 缺省的 3 个动作（`A:~确定:2`、`A:~确定:3`、`A:~确认下段:1`，以及 `B:~打印:3`、`B:~关闭:6`、`C:~妥投确认:1`）
  我按 **`attached`（元素存在即可）** 实现。依据：这些动作的 `ui_delta.added` 里正好是 `#modalMask` 从无到有，
  而 `hidden`/`visible` 都会更强；包里没有定义缺省值语义。→ **待确认：是不是应该按 `visible` 等？**
- `state=value_set`（`A:~input#mailNo:1/2`、`B:~on:2`、`C:~input#returnMailNo:1/2`、`B:~input#phoneInput:1`）
  我用一条 JS 断言实现：`checkbox/radio` 看 `checked`，其它看 `value` 非空。→ **待确认：原探针的 `value_set` 判定口径**
  （比如是否要求“等于刚写入的值”，我只做了非空/已勾选）。
- `type=request` 的 3 个动作（`C:~退回寄件人:2`、`C:~退回寄件人:3`、`D:~→:1`）
  我用 `page.expect_response(method + path 子串)` 包住点击，超时即失败。这与 ACT 的 `type=request` 一致。

### 2.2 `next_waits_for` 里的元素有 6 个不在 `SELECTORS.json`

`#modalMask`、`#panel-B`、`#panel-C`、`#panel-D`、`li`、`td`。
也就是说：**“点完之后等什么”这件事，包里没有给稳定性评级**。我按原样使用（这是 `next_waits_for` 的要求），
但它们确实是脚本里最脆弱的一类条件：

- `li`（`A:~确定:1`）：这条其实**有意义**——初始页 `<ul id="scanList">` 是空的，只有点“确定”之后才追加“xxx 已录入”。
  但要注意 `PAGES/A-下段扫描.html` 是**第 3 次扫描之后**的快照（已有 3 个 li），别误以为它是静态内容。
- `td`（`B:~710000000004140,7100:1`、`B:~查询:1`）：初始 `tbody` 为空，`td` 只有点“查询”后才渲染。
  也就是说 `B:~710000000004140,7100:1` 的 `td` 等待**很可能在真机超时**，需要确认“输入单号串后是否自动查询”。
  另一种可能是探针把等待错位到了下一步（`B:~查询:1` 才是产生 `td` 的那一步）。
  **已在包内 mock 上实测确认它会超时**（§10），所以脚本为这一个动作加了 `WAIT_RECOVERY`：
  记录等待失败后，退到“`#printNos` 有值”这个更弱但语义正确的条件（`--strict-waits` 可关掉恢复）。
- `#panel-B/C/D`（3 个 Tab 动作）：面板本来就在 DOM 里（只是 `hidden`），`attached` 等待**恒真**，
  所以 Tab 点击是否成功没有任何校验。我保留原语义，没有擅自改成 `visible`——
  **建议人工确认后把 Tab 的等待改成 `#panel-X visible`**（改一处即可）。
- `#modalMask hidden`：依赖 mock/真机用 `display:none`（`.hidden`）。若真机用 `visibility/opacity`，
  Playwright 的 `hidden` 状态可能一直不等不到。→ 实机先跑一遍 A 阶段验证这一点。

### 2.3 请求成功判定

API.md 的 8 个样本响应**全部**是 `{"code":0,"msg":"成功",...}`。脚本把 `code != 0` 记为 WARN、不中断。
→ **待确认：`code=0` 是否就是唯一成功码；业务失败长什么样（msg？code 其它值？）** 包里没有反例样本。

### 2.4 “等待目标 = 本次点击的元素，且要求 visible” —— 自相矛盾的等待

有 7 个动作属于这一类：`A:~关闭:2`、`B:~关闭:1`、`B:~关闭:2`、`B:~关闭:3`、`B:~on:1`、`B:~打印:1`、`B:~打印:2`
（`next_waits_for` 都是 `#modalClose`/`#checkAllTop`/`#btnPrint` + `visible`，而 `selector.primary` 就是同一个元素）。
点“关闭”之后 `#modalClose` 必然不可见——按字面执行，等待必然失败（第一次跑就是这样卡住的，见 §10）。

脚本处理：**这类等待按“前置条件”处理**——点击前已经断言过 visible，点击后不再重复等待，
日志会打 `wait 视为前置条件（目标=本次点击元素…）`。判定是自动的（`wait.state=='visible' and wait.selector==action.selector`），
没有白名单。
→ **待确认**：探针记录这条 `next_waits_for` 时，等的到底是“点击后的状态”还是“下一个动作的前置状态”。
若是后者，说明 `next_waits_for` 的语义整体要重新定义（那会影响全部 37 条）。

---

## 3. 动作语义不确定

### 3.1 `A:点确定:1` —— 三条证据互相矛盾

- ACT：`kind=change`，定位 `#mailNo`，`next_waits_for=#modalMask hidden`，`ui_delta.removed=modalMask`；
- 标注：`annotation.mode=hotkey`、`action_no=2`、名字“点确定”（`RAW/annotations.csv` 也只有这 1 行标注）；
- SOPDIFF：A 阶段“输入邮件号”写着**未记录到**。

我的处理：按 `change` 处理，即“往 `#mailNo` 填第 1 个单号”，值来自 `--data`。
→ **待确认**：这一步到底是①往输入框打字、②按回车提交、③点“确定”按钮？若是②，需要在填值后补 `press("Enter")`
（脚本里没有加，因为记录里没有任何键盘事件）。

### 3.2 B 阶段“打印/关闭/手机号”是循环，记录只覆盖 1 轮

ACT 只记录了：打印 ×4（`B:~打印:1..4`）、关闭 ×6（`B:~关闭:1..6`）、1 次手机号（`B:~input#phoneInput:1` + `B:~确定:1`）。

MOCK 的对应逻辑（只能当线索）：
- 第 1 次打印 → `alert`；第 2 次 → 什么都不弹；第 3 次 → **排队 5 个 DOM 提示框**；第 4 次起 → 弹“填写收件人手机号”，
  提交后提示“请再点一次打印”，`phoneLoop` 最多 3 轮。
- 这正好解释“5 个提示框 vs 6 次关闭”的错位：多出来的那 1 次关闭点的是已隐藏的 `#modalClose`。

由此产生 3 个必须人工确认的问题：
1. **手机号要循环几轮？** 是“每个单号一次”（3 个单号 → 3 轮）还是“每次打印一次”？记录只有 1 轮，
   SOPDIFF 也没有这一项。ASR 里 `B_print_phone_loop` 的 `observed` 甚至是“B 阶段 DOM 模态框 0 个”（与 ACT 的
   `#phoneInput` 记录直接冲突）。
2. **6 次关闭的实现方式**：我按记录逐条重放（都标 `optional=True`，等待不到就跳过并告警）。
   如果真机确实是“队列式”弹窗，实际应该写成“关闭直到 `#modalMask` 变 hidden，最多 N 次”。
   脚本目前**没有**这样写，因为那会偏离 `next_waits_for` 的逐条语义。→ 建议实机确认后改写这一段（只影响 §3.2）。
3. **模板选择与“确定打印”完全没被记录**：PG 里有 `<select id="template">`（`gnkb-zd` 正式 / `gnkb-cs` 测试）
   和 `<button id="btnConfirmPrint">确定打印</button>`，ACT 里**一个动作都没有**，
   `SEL` 里也没有 `#template`、`#btnConfirmPrint`。而 SOPDIFF 写着“选打印模板：文档 0-3 / 实际 4”。
   → 这条差异无法靠现有数据落地，`automation.py` **不点模板、不点“确定打印”**（MOCK 里它会发 `POST /api/print-confirm`，
   而 API.md 根本没有这个接口）。→ **必须人工确认：模板选哪个？什么时候点“确定打印”？**

### 3.3 `C` 阶段“逐单输入 Enter”没有键盘事件

SOPDIFF 的文档第 14 步是“逐单输入 Enter”，记录里只有 `change`（`C:~input#returnMailNo:1/2`）+ 点按钮
（`C:~退回寄件人:2/3`）。脚本按“填值 → 点退回寄件人”推进，不模拟 Enter。
→ **待确认**：真机是否支持 Enter 提交（若支持，可能更快，但 Enter 与按钮的副作用是否等价未知）。

### 3.4 JS 原生对话框：`confirm` 点“是”还是“取消”？

- DLG：`seq=151` 点“妥投确认” → `confirm("确认将选中记录标记为妥投？")`；`seq=87` 点“打印” → `alert("打印任务已提交（模拟）")`。
- ACT/`next_waits_for` 里**完全没有这两类对话框**，也就是说 ASR/ACT 都没建模它们。
- 脚本做法：注册 `page.on("dialog")`，`alert→accept`，`confirm→accept`，并把每条对话框记进日志。
- **风险**：`confirm` 选“是”会把记录标记为妥投，属于**不可逆业务动作**。包内唯一的间接证据是
  “`seq=151` 之后确实出现了 `POST /api/delivered`”（API.md 样本 1/1 成功）。
→ **建议**：首次实机运行先 `--headed` 人工盯 B/C 阶段，或临时把 `confirm` 策略改成“停下等人工确认”。

### 3.5 `B:~on:1` / `B:~on:2` 是同一个控件的 click + change

`#checkAllTop` 在 SEL 里 occurrences=2、sources=[click, change]。脚本：`click` 步若识别为 checkbox/radio 就调 `check()`；
后面的 `change` 步（`B:~on:2`）**不重复操作**，只按 `value_set` 断言“已勾选”。
→ **待确认**：真机上“全选”之后是否还需要其它确认（例如行数校验）。另外 MOCK 里 `checkAllTop` 没有 `change`
监听，记录里的 `change` 更像是探针自己派发的。

---

## 4. 定位器风险（`unique:false` 与“只在 backup 里”的 SEL 条目）

SEL 一共 15 条，`unique:false` 只有 1 条（`B.详情单补打` → `button.tab`，MISSING 也点了名）。
但把 ACT 的 37 个 `selector.primary` 对一遍，会发现还有 2 个动作的 `primary` **只在 SEL 的 backup 里出现**：

| action_uid | primary | SEL 里是什么 | 风险 |
|---|---|---|---|
| `B:~详情单补打:1` / `C:~退回寄件人:1` / `D:~投递归班交接:1` | `button.tab` | `B.详情单补打`（unique=false、stability=low） | 4 个 Tab 共用，必须靠文本收敛 |
| `B:~确定:1` | `button.btn` | 只作为多条的 backup（`A.确定` 等的第 2 个备选） | B 页面至少 4 个 `.btn` 可见 |
| `C:~关闭:1` | `button.btn` | 同上 | 模态页脚“关闭” |

### 4.1 运行期消歧（脚本已实现）

`_locate()` 的规则，**按顺序**：
1. `primary` 命中 1 个 → 直接用；
2. 命中 >1 个 → 只保留 **`is_visible()` 为真** 的候选，再按文本：优先 `inner_text == 动作名（去掉“~”前缀）`，
   其次 `inner_text` 包含动作名；仍不唯一 → 抛 `NeedHumanDecision`（退出码 2，并打印候选特征）；
3. 命中 0 个 → 依次试 ACT 的 `selector.backup`（其中包含结构路径与 XPath 文本），命中唯一就用，并打 `FALLBACK` 告警。

这套规则能在 mock 结构下把 4 个 Tab、`#btnConfirmPrint` 干扰、C 页脚“关闭”都收敛到 1 个，
但**它依赖“页面文字”**，属于脚本里唯一“半猜”的地方。

### 4.2 更稳的替代（建议人工确认后改）

RPT 的 `q3.control_map` 记录到 `button[data-tab='C']`、`button[data-tab='D']` 是 `unique:true`，
PG 的 `<button class="tab" data-tab="A|B|C|D">` 也确认了这 4 个属性存在。
**但它们没有进 `SELECTORS.json`**（SEL 只有 `button.tab` 这一条），所以我**没有**在脚本里用 `data-tab`。
→ 如果人工确认 `data-tab` 在真机上稳定，建议把它补进 SEL，然后 Tab 的定位可以改成 `button[data-tab='X']`，
§1 里 3 个 Tab 动作的“中”立刻变“高”。

### 4.3 结构路径不要用

ACT 的 backup 里有 `html > body:nth-child(2) > div:nth-child(1) > nav:nth-child(2) > button:nth-child(2)` 这类路径。
MISSING 已经说了“页面改版易断”。脚本只把它们当**最后兜底**（§4.1 第 3 步），并且每次命中都会打 WARN。

---

## 5. 覆盖缺口

### 5.1 D 阶段（投递归班）只有 2 个动作，SOP 却有 5 步

SOPDIFF 明确：文档第 18/19/20 步（选投递员、点查询、点确定）**未记录到**；第 17/21 步（打开投递归班、点 “→”）有记录。
PG-D 里也确实只有“当前批次”文本 + `#btnHandover`，没有任何投递员下拉/查询控件。
所以 `automation.py` 的 D 阶段只有“切 Tab + 点 →”，**另外 3 步是 TODO 占位**（`DOC_STEPS` 表里标 `missing`，
`--selfcheck` 会以 WARN 打出来）。
→ **必须补采**：这 3 步要么真机不存在（那 SOP 要删），要么采集缺失。二者都会改变 D 阶段的实现。

### 5.2 文档漏项（实际有、文档没写），共 10 条（SOPDIFF）

`seq 35/46`（A 阶段打字）、`seq 72`（B 粘贴单号串）、`seq 83/85`（B 全选）、`seq 119`（B 手机号）、
`seq 133/140`（C 打字）、`seq 164`（D 切 Tab）、`seq 166`（D 点 →）。
这些在脚本里都有对应动作，**不影响实现**；但说明 SOP 文档需要改写（否则人工复核会对不上）。
另有 2 处次数不符：**选打印模板 0-3 vs 4**（见 §3.2）、**点退回寄件人 1-1 vs 3**（记录里是
`C:~退回寄件人:1` Tab + `:2`/`:3` 两次按钮，文档把 Tab 和按钮算混了）。LCS 覆盖率 0.38，SOPDIFF 自己也说“置信度低”。

### 5.3 有记录请求、但本次没观测到请求时是否阻断？

10 个动作带请求记录（`A:~确定:1..3`→`/api/scan`、`A:~确认下段:1`→`/api/scan-confirm`、
`B:~查询:1`→`/api/print-query`、`B:~确定:1`→`/api/print-phone`、`C:~退回寄件人:2/3`→`/api/return`、
`C:~妥投确认:1`→`/api/delivered`、`D:~→:1`→`/api/handover`）。
脚本对“等待是元素、但记录里有请求”的 7 个动作，在等待之后**轮询 5 秒看请求是否出现**，没出现只 WARN 不中断。
→ **待确认**：这些请求是否应当作为“该步成功的唯一判据”（若是，应改成硬失败）。

---

## 6. 数据、地址与打印（跑不起来的现实原因）

### 6.1 单号/手机号全部脱敏

`RAW/inputs.json` 只有 `hash8` + `7100***********`；API.md 的 body 也是脱敏样本。
因此 `automation.py` 强制要求 `--data`：

```json
{
  "scan_mail_nos":  ["<A 阶段 3 个单号>"],
  "print_nos":      "<B 阶段逗号串，记录为 3 个单号>",
  "receiver_phone": "<11 位手机号，记录为 130******00>",
  "return_mail_nos":["<C 阶段 2 个单号>"]
}
```

数量是按记录反推的：A 阶段 3 次 `/api/scan` × 1 单号/次（EXP21：一次点击只发 1 个单号、无批量接口），
C 阶段 2 次 `/api/return`，B 阶段 1 次 `/api/print-query` 带 3 个单号。
检测到 `*`、手机号格式不对、数量不符 → `--selfcheck/--dry-run` 打 WARN，真跑直接 FAIL 退出。

### 6.2 真实地址没有记录

RPT 只记录了本地 mock `http://127.0.0.1:8080`。脚本因此**没有默认 URL**：不给 `--url` 时只打印自检报告。
→ **待确认**：内网真实地址、登录态怎么来（账号密码 / 已有浏览器配置 / Cookie 注入）。
脚本目前直接 `page.goto(url)`，**没有任何登录逻辑**——这是“能自动完成”的另一个硬缺口。

### 6.3 打印

- RPT 记录默认打印机 `RICOH M C2001 PCL 6`（非虚拟），`q5.native_count=0`、`print_preview_count=0`、
  `spool_events=[]` → 采集期间**没有观察到任何真实的打印预览/打印窗口**，只有 JS `alert`。
  MOCK 里打印就是 `alert` + 状态文字，**从不真的打印**。
- 所以：真实平台点“打印”会不会打开浏览器原生打印对话框 / 打印预览，**包里没有证据**。
  若会，`page.on("dialog")` 管不到原生窗口，需要在启动参数里做（例如 Edge 的 `--kiosk-printing`）或走 PDF 方案。
- → **待确认**：①真机“打印”后的实际形态；②是否允许静默打印；③用哪台打印机；④打印失败怎么补偿（重打/对账）。
- 脚本层面：`--headed` 是必须的（`headless` 下原生打印窗口行为与有头不同），并且**不对打印结果做校验**（记录里也没有校验点）。

### 6.4 环境

- RPT：`Edge 157.0.4341.60`、`UA ... Edg/157`，风险条目写的是“Edge 升级后 CDP 细节可能变化，建议固定版本”。
- 脚本用 `channel="msedge"`；若目标机 Edge 版本/通道不一致，需要改 `--channel` 或装 Playwright 自带 Chromium。
- 本环境验证状态：`python -m py_compile` 通过、`--selfcheck` 通过（FAIL 0）、`--dry-run` 全 37 步可打印、
  **对着包内 mock 冒烟跑通 37/37 步**（见 §10）；
  **没有**在真实内网系统上跑过（本机无该系统，真机验证需在目标机做）。

---

## 7. 需要人工拍板的清单（按优先级）

| # | 问题 | 影响 | 确认方式 |
|---|---|---|---|
| 1 | B 阶段手机号要循环几轮？每轮是否都要再点一次“打印”？ | B 能否跑完 | 真机操作一遍 + 抓 `/api/print-phone` 次数 |
| 2 | 打印模板选哪个？什么时候点“确定打印”（`#btnConfirmPrint` → `/api/print-confirm`）？ | B 结果对不对、单据会不会错 | 问操作员 + 抓包；确认后补进 ACT/SEL |
| 3 | D 阶段的“选投递员/查询/确定”到底存不存在？ | D 阶段完整性 | 真机截图 + 补采 |
| 4 | `confirm("确认将选中记录标记为妥投？")` 该点“是”还是“取消”？ | 不可逆业务动作 | 问操作员 |
| 5 | `data-tab` 能否作为 Tab 的稳定定位器（补进 SEL）？ | 4 个 Tab 的稳定性 | 真机看属性是否固定 |
| 6 | `#modalMask` 的隐藏是 `display:none` 吗？`#panel-X` 的等待是否该改成 `visible`？ | 等待是否可靠 | 实机跑 A 阶段一遍 |
| 7 | 输入单号串后是否自动查询（否则 `td` 等待必超时）？ | B 第一步 | 实机 |
| 8 | 登录态怎么获得？ | 全流程自动化 | 问运维 |
| 9 | 打印到底会不会弹原生窗口？要不要静默打印？ | B 能否无人值守 | 实机点一次“打印” |

---

## 8. 与包内其它材料冲突的点（提醒，不要当成实现依据）

1. **ASR 与 ACT 冲突**：`A_scan_confirm` 的 `observed` 是“未观测到 A 阶段弹窗”，但 ACT 里 `A:~确定:1..3`、
   `A:~确认下段:1` 都记录了 `#modalMask` 出现；`B_print_phone_loop` 的 `observed` 是“B 阶段 DOM 模态框 0 个”，
   但 ACT 里有 `B:~input#phoneInput:1`。脚本以 ACT 为准（ASR 自己也写了 pass=false，像是探针没捕捉到）。
2. **DLG 里的 2 个原生对话框在 ACT/SEL 里完全缺失**（§3.4）。
3. **MOCK 有 `/api/print-confirm`，API.md 没有**；API.md 只列了 8 个接口。若真机也有这个接口，
   说明本次采集漏了“确定打印”这一步。
4. **SOPDIFF 说 A 阶段“输入邮件号”未记录**，但 `A:点确定:1` 就是一个 `change@#mailNo`（§3.1）。

---

## 9. 如果要把骨架变成“能无人值守跑”的下一步

1. 补齐 §7 的 1/2/3/4 四个答案 → 改写 `PLAN_B`、`PLAN_D`（只动计划表，运行器不用改）。
2. 用 `data-tab` 替换 Tab 的文本收敛（§4.2），并把 `#template`、`#btnConfirmPrint` 补进 SEL。
3. 把 §2.2 里 6 个“等待元素”补进 SEL（给 stability 评级），并把 `#panel-X` 的等待改成 `visible`。
4. 打印策略单独立项（§6.3），建议先做“拦截 + 落 PDF”再谈自动打印。
5. 断点续跑与失败重试：`PLAN` 是线性重放，没有事务边界；建议以 action_uid 作为断点键
   （`seq` 会随重采集变化，不能当键——这也是 README 要求用 uid 的原因）。

---

## 10. 冒烟记录（只证明“脚本逻辑自洽”，不证明“真机可用”）

做了一次本地冒烟：把 `RAW/mirror/127.0.0.1_8080/`（mock 静态站）用 `http.server` 起在 `127.0.0.1:8099`，
并按 API.md 的 8 个接口签名给 `/api/*` 打桩（一律返回 `{"code":0,"msg":"成功",...}`），
然后用 `playwright + msedge(headless)` 跑 `automation.py --url http://127.0.0.1:8099/ --data <mock 数据>`。

| 观测项 | 结果 |
|---|---|
| 动作覆盖 | 37/37 STEP 全部执行，退出码 0 |
| 请求 | 观测到 10 个 `POST`，与 ACT 里 10 个“带请求动作”一一对应（scan×3 / scan-confirm / print-query / print-phone / return×2 / delivered / handover） |
| JS 对话框 | 2 个，均由 `page.on("dialog")` 处理：`alert 打印任务已提交（模拟）`、`confirm 确认将选中记录标记为妥投？` |
| 非唯一 Tab | 3 次全部靠“可见 + 文本”收敛成功（`button.tab` 命中 4 个 → 1 个） |
| `button.btn` | `B:~确定:1` 命中 9 个 → 收敛到模态页脚“确定”；`C:~关闭:1` 同样收敛到“关闭” |
| 自相矛盾等待 | `A:~关闭:2`、`B:~关闭:1..3` 按 §2.4 处理，未失败 |
| `td` 等待 | `B:~710000000004140,7100:1` 的 `td` 等待**确实超时 5s**，触发 `WAIT_RECOVERY` 后继续 |
| 模态队列 | `B:~打印:3` 的 5 个排队提示框被 `B:~关闭:1..5` 关完；`B:~关闭:6` 因目标不可见而 `SKIP(optional)`（正好是多出来的那 1 次） |
| D 阶段 | `POST /api/handover` 成功，`#panel-D` 变成“已交接” |

**这些数字只对 mock 成立**：mock 是按“真实平台可能的样子”写的，不是真机。
真机可能在第 1 个 Tab、第 1 个打印弹窗、第 1 次 confirm 上就不一样（§3.2 / §3.4 / §7）。
冒烟的价值在于：证明“计划表 + 等待 + 定位器消歧 + 对话框处理”这套骨架自身没有逻辑错误，
剩下的不确定性全部集中在 §7 的 9 个人工确认项上。
