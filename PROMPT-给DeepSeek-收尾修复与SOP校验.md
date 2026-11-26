# 任务：定点修复 6 个缺陷 + 新增「SOP 一致性校验」

> 给 AI 编码助手的施工 prompt。**这是收尾轮，不是新功能轮。**
> 前五个模块已交付、67 个测试全绿、真 agent trial 已跑过。下面每一条都是**实测发现的具体缺陷**，
> 不是设计推演。改动量估计 500-700 行。

---

## 0. 这轮要解决什么

上一轮把探针从"诊断器"改成了"影印机"，采集层扎实了。但真 agent 试写暴露了编译层的三个 bug，我复核时又找到三处。

同时补上一个**一直被漏掉的目标**：

> 现有逻辑是「操作员照 SOP 做，探针记录她做了什么」。
> 如果她本来就是照 SOP 做的，探针永远发现不了 SOP 有错。
>
> **要验证 Word 文档对不对，必须把文档变成期望序列，跟实际序列比对，输出差异。**

这就是本次新增的第六个模块：`sopdiff`。

---

## 1. 已复核的缺陷清单（按优先级）

| # | 缺陷 | 证据 | 危害 |
|---|---|---|---|
| **1** | 脱敏值漏进 selector | `//input[normalize-space()='7100***********']`，`SELECTORS.json` 里有 key `A.7100***********` | 生成的脚本 100% 匹配失败，且**伪装成 `unique:true, stability:high`** |
| **2** | `MISSING.md` 空节写反 | `## 完全缺失` 下面写「响应体全部拿到了」 | agent 先读这节，会被误导 |
| **3** | 纯输入框不进 `SELECTORS` | `#printNos`、`#phoneInput` 不在表里 | 阶段 B 核心动作无定位器，agent 只能 TODO |
| **4** | `ui_delta` 方向颠倒 | 33 动作中 15 个 `next_waits_for.type == "none"`；added 15 / removed 29 明显不对称 | 45% 的动作没有可用等待条件 |
| **5** | `seq` 不可作稳定 ID | 重生成包后 seq 整块重编号 | 引用失效 |
| **6** | 退化标签进表 | key `B.on`，`primary=#checkAllTop` | 噪音，误导 agent |

---

## 2. 修复 1：selector 生成禁止使用脱敏值（P0）

### 2.1 根因

`elements.json` 里的文本/值**已经脱敏**（`redact::mask_number` 保留前 4 位）。`agentpack` 从这些值拼 xpath 文本匹配，于是拼出含 `***********` 的选择器。

更糟的是它在 mock 上**能匹配**（因为 mock 页里真的有那个值），所以 `unique` 校验通过了。真机上值不同，必然失败。

### 2.2 修改点

`src/agentpack/selectors.rs` 与 `src/agentpack/actions.rs` 里生成 selector 候选的所有位置。

```rust
/// 脱敏特征：任意位置出现连续 2 个以上的 '*'
pub fn looks_masked(s: &str) -> bool {
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == '*' { run += 1; if run >= 2 { return true; } }
        else { run = 0; }
    }
    false
}

/// 从元素生成 selector 候选。**任何用到文本/值的候选，值必须先过 looks_masked。**
pub fn candidates(el: &Value) -> Vec<Candidate>;
```

**规则（按顺序，命中即止）：**

1. `id` 非空且不含数字长串 → `#{id}`，`why = "id"`
2. `name` 非空 → `[name='{name}']`，`why = "name"`
3. `data-*` / `aria-label` / `placeholder` 非空且 **`!looks_masked`** → 属性选择器，`why = "attr"`
4. 元素文本非空、`!looks_masked(text)`、长度 ≥ 2 → `//tag[normalize-space()='{text}']`，`why = "text"`
5. 上述全不成立 → CSS path（位置路径），`why = "path"`，**强制 `stability = "low"`**

**逐条硬要求：**

- **凡是要把「值」写进 selector 的地方，一律先 `looks_masked` 过滤。** 值包括 `element.text`、`element.value`、`element.attrs.value`、`input` 的当前内容。
- 被过滤时**不要生成候选**，不要留空字符串占位。
- `SELECTORS.json` 的 `key` 命名同样受限：`format!("{}.{}", step, name)` 里 `name` 若 `looks_masked`，改用元素 `tag` + `id`，例如 `A.input#mailNo`。
- 修完后全局自检：`grep -c '\*\*' SELECTORS.json ACTIONS.json` 必须为 **0**（在 mock 上跑一遍验证）。

### 2.3 新增：`element_ref`

过滤掉脱敏候选后，有些元素只剩位置路径。给每个动作补一个可核对引用：

```json
"selector": {
  "primary": "#mailNo",
  "backup": ["input[name='mailNo']"],
  "element_ref": {"page": "A", "tag": "input", "id": "mailNo", "seq_in_page": 12},
  "unique": true,
  "note": null
}
```

`element_ref` 指向 `PAGES/A-下段扫描.html` 里的第 12 个 `input`，**绝对稳定**，永远不会因为脱敏而失效。

### 2.4 单元测试（≥3 个）

1. `looks_masked("7100***********") == true`，`looks_masked("710000000004161") == false`，`looks_masked("a*b") == false`
2. 构造一个 `text` 含脱敏星号的元素 → 生成的候选里**不含任何文本选择器**，且 `stability == "low"`
3. `SELECTORS.json` 序列化结果不含连续两个 `*`

---

## 3. 修复 2：`MISSING.md` 空节处理（P0）

`src/agentpack/missing.rs`：任一分类为空时，**不要写"无缺口"之外的任何解释性文字**。

```markdown
## 完全缺失

无

## 部分缺失

无
```

**禁止**出现「响应体全部拿到了」「一切正常」「已确认完整」这类表述——它们会被 agent 当成结论。

只有一类例外：`## 未语义标注` 在 `annotated == total` 时整节省略（不写"无"）。

### 单元测试（≥2 个）

1. 全部缺口为空 → 输出含「无」且**不含**「拿到了」
2. `annotated == total` → 输出不含 `未语义标注` 这个标题

---

## 4. 修复 3：把输入控件并进 SELECTORS（P1）

### 4.1 修改点

`src/agentpack/selectors.rs`：聚合来源从「只有 click 目标」扩到三个：

1. `click` 事件的 `element`（现状）
2. **`change` 事件的目标元素**（新增）
3. **`elements.json` 的 `control_map`** 里有 `unique:true` 的项（新增）

### 4.2 去重与合并

同一物理控件可能同时出现在三个来源里。合并键优先级：

```
id 非空 → "id:{id}"
否则 name 非空 → "name:{name}"
否则 → "sig:{tag}|{cls}|{rect.x},{rect.y}"
```

合并后 `occurrences` 累加，`sources` 记录来源数组：

```json
"sources": ["click", "change", "control_map"]
```

### 4.3 输入框的额外字段

对 `tag == "input" || tag == "textarea"` 的条目，补：

```json
"input_kind": "text|number|tel|checkbox|select",
"observed_values": [{"masked": "7100*********161", "hash8": "a3f5c1d2", "seq": 24}],
"maxlength": 21
```

`observed_values` 来自 `timeline` 的 `input_number` 事件（若上轮没实现，从 `change` 事件的 `tokens` 里取）。**`maxlength` 一定要记**——这是 Q2（"21 到底是不是限制"）的第一手证据。

### 4.4 验收

- [ ] mock 上 `#printNos`、`#phoneInput` 出现在 `SELECTORS.json`
- [ ] 它们的 `sources` 含 `change` 或 `control_map`
- [ ] 同一个按钮点了 12 次仍只有 1 条（`occurrences: 12`）
- [ ] 输入框条目有 `maxlength` 字段（mock 的 `#mailNo` 为 `null`，`?mode=h1` 下为 `21`）

---

## 5. 修复 4：`ui_delta` 方向颠倒（P1，本轮的硬骨头）

### 5.1 根因（已定位）

`record_click.js` 的 `click` 监听是**捕获阶段**（`addEventListener(..., true)`），但 `Runtime.bindingCalled` 送到 Rust 侧要过 WebSocket，而页面自己的 handler 可能**同步执行完毕**。等 Rust 去截"前"图时，弹窗已经出来了。

结果：`before` 拍到的是"之后"，`after` 拍到的是"更之后"，`added`/`removed` 全部错位。

### 5.2 修法：DOM 在页面内同步取，截图用录屏环形缓冲

**(a) DOM before：在 JS 里同步序列化**

`assets/record_click.js` 增加：

```js
// 捕获阶段，pointerdown 比 click 更早
document.addEventListener('pointerdown', function (e) {
  try {
    var html = document.documentElement.outerHTML;   // 同步，此刻 DOM 还没变
    send('pointer_down', {
      element: describe(e.target),
      dom_html_len: html.length,
      dom_html: html.slice(0, 3000000)               // 3MB 上限，超出标记
    });
  } catch (err) {}
}, true);
```

- `pointerdown` 在业务 handler 之前触发，且**同步**取 HTML
- Rust 侧收到这条事件时，内容**已经是点击前**的
- 体积：企业页 100KB-1MB，3MB 上限够用；超出时在末尾加注释并记 `dom_truncated: true`
- **注意**：`pointerdown` 之后可能被 `preventDefault` 取消，导致 `click` 不触发。所以 `before` 快照要挂在 `pointerdown` 上，`after` 仍然挂在 `click` 上，两者用同一个 `action_seq` 关联。孤立的 `pointerdown`（没有对应 click）在 2 秒后丢弃并记 `note`。

**(b) 截图 before：`Page.startScreencast` 环形缓冲**

不要再"收到事件后去截"，改成**一直录屏**：

```rust
// 每个 page session 启动时
cdp.call("Page.startScreencast", json!({
  "format": "jpeg", "quality": 70,
  "maxWidth": 1280, "maxHeight": 720, "everyNthFrame": 2
}), Some(sid)).await?;

// 订阅 Page.screencastFrame，维持最后 30 帧的环形缓冲（约 3-5 秒）
// 每帧带 CDP 给的 metadata.timestamp
```

收到 `pointer_down` 时：**取缓冲区里时间戳 <= 该事件时间戳的最后一帧**作为 `before`，存盘。
收到 `click` + 延迟 400ms 后：正常截一张作为 `after`。

- 缓冲上限 30 帧 × 约 60KB = 1.8 MB 内存，可接受
- 帧落盘时才写文件，内存里只留 JPEG 字节
- **`--no-filmstrip` 时 `startScreencast` 也不开**（省 CPU）
- 若 `Page.screencastFrame` 在 3 秒内一帧都没来（某些环境不支持），降级为：直接用 `pointer_down` 时刻同步 CDP 截图，并在记录里标 `before_method: "late_best_effort"`

**(c) `ui_delta` 改为基于 `pointer_down` 的 DOM 与 `after` 的 DOM 做 diff**

即 `delta = diff(dom_at_pointerdown, dom_after_400ms)`。这样方向和业务语义一致。

### 5.3 验收

- [ ] mock 上点"确定"弹窗：`ui_delta.added` 含弹窗节点，`removed` 为空
- [ ] 点"×"关闭：`ui_delta.removed` 含弹窗节点，`added` 为空
- [ ] 全局统计 `added` 与 `removed` 不再是 15:29 的失衡
- [ ] `next_waits_for.type == "none"` 的比例从 45% 降到 **< 20%**
- [ ] 任一 `before` DOM 与 `after` DOM 的 `document.documentElement.outerHTML` 长度差 > 0 的动作，`ui_delta` 非空
- [ ] `--no-filmstrip` 时不调 `Page.startScreencast`（用 CDP 日志或 mock 计数验证）

---

## 6. 修复 5：跨会话稳定的 `action_uid`（P2）

`src/agentpack/actions.rs`：

```rust
/// 跨会话稳定标识：阶段 + 动作名 + 该动作在本次会话内的出现次序。
/// 与 seq 无关 —— seq 每次重生成都会变。
pub fn action_uid(step: &str, name: &str, nth: usize) -> String {
    format!("{}:{}:{}", step, name.trim(), nth)
}
```

写入 `ACTIONS.json` 每个动作：

```json
{"action_uid": "B:点打印:1", "seq": 42, ...}
```

规则：
- `nth` 从 1 开始，**同一 `(step, name)` 组合内**计数
- `name` 若来自推断（无 annotation），前缀加 `~`：`"B:~按钮8:1"`，并标 `"name_from": "inferred"`
- `00-AGENT-READ-ME.md` 里加一句：「引用动作请用 `action_uid`，不要用 `seq`。`seq` 只在本包内有效。」

### 验收

- [ ] 同一份 `out` 跑两次 `agentpack`，`action_uid` 集合完全相同
- [ ] 改一个无关文件（如 `report.md`）后重新采集，`action_uid` 仍相同

---

## 7. 修复 6：过滤退化标签（P2）

`SELECTORS.json` 出现 `B.on`、`primary=#checkAllTop`。成因：开关组件的状态文本（`on`/`off`）被当成控件名。

**过滤规则（`agentpack/selectors.rs`）：**

控件名 `name` 满足下列任一，改用元素 `tag` + `id`/`class`：

1. 去空白后长度 < 2
2. 命中状态词表：`on|off|true|false|是|否|启用|禁用|开|关`
3. 纯数字
4. `looks_masked`

同时 `describe()` 在 JS 侧取文本时**优先 `aria-label` > `title` > `placeholder` > `innerText`**，减少状态文本污染。

### 验收

- [ ] `SELECTORS.json` 里不再出现 `on`/`off` 之类的 key
- [ ] 该控件仍然在表里（只是换了 key），不能因为过滤而丢条目

---

## 8. 新增模块：SOP 一致性校验（`src/sopdiff.rs`）

### 8.1 为什么

**这是你这轮最想要的东西：验证 Word 文档本身对不对。**

现有链路是「人照文档做 → 探针记录」。如果她照做，探针永远发现不了文档的错误——比如文档写了某个按钮，实际没有；文档漏了某个必须的确认框。

### 8.2 `plan.json` 增加 `sop_flow`

```json
{
  "sop_flow": {
    "version": 1,
    "source": "自动退货处理.doc",
    "steps": [
      {"no": 1, "step": "A", "name": "输入邮件号", "repeat": {"min": 1, "max": 21}, "required": true},
      {"no": 2, "step": "A", "name": "点确定",     "repeat": {"min": 1, "max": 21}, "required": true},
      {"no": 3, "step": "A", "name": "点确认下段", "repeat": {"min": 1, "max": 1},  "required": true},
      {"no": 4, "step": "B", "name": "点详情单补打", "repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 5, "step": "B", "name": "粘贴单号串", "repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 6, "step": "B", "name": "点查询",     "repeat": {"min": 1, "max": 3}, "required": true},
      {"no": 7, "step": "B", "name": "勾选全选",   "repeat": {"min": 1, "max": 3}, "required": true},
      {"no": 8, "step": "B", "name": "选打印模板", "repeat": {"min": 0, "max": 3}, "required": false},
      {"no": 9, "step": "B", "name": "点打印",     "repeat": {"min": 1, "max": 50}, "required": true},
      {"no": 10,"step": "B", "name": "点确定打印", "repeat": {"min": 1, "max": 50}, "required": false},
      {"no": 11,"step": "B", "name": "关闭弹框",   "repeat": {"min": 0, "max": 999},"required": false},
      {"no": 12,"step": "B", "name": "填手机号",   "repeat": {"min": 0, "max": 999},"required": false},
      {"no": 13,"step": "C", "name": "点退回寄件人","repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 14,"step": "C", "name": "逐单输入Enter","repeat": {"min": 1, "max": 50},"required": true},
      {"no": 15,"step": "C", "name": "点妥投确认", "repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 16,"step": "C", "name": "点关闭",     "repeat": {"min": 0, "max": 3}, "required": false},
      {"no": 17,"step": "D", "name": "打开投递归班","repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 18,"step": "D", "name": "选投递员",   "repeat": {"min": 1, "max": 1}, "required": true},
      {"no": 19,"step": "D", "name": "点查询",     "repeat": {"min": 1, "max": 3}, "required": true},
      {"no": 20,"step": "D", "name": "点确定",     "repeat": {"min": 1, "max": 3}, "required": true},
      {"no": 21,"step": "D", "name": "点 ->",      "repeat": {"min": 1, "max": 1}, "required": true}
    ]
  }
}
```

**这张表是初稿，来自 SOP。DeepSeek 照抄，允许用户后续改。**

### 8.3 比对算法

输入：`sop_flow.steps`（期望）+ `annotation` 事件序列（实际）

**匹配规则**：按 `(step, action_no)` 精确匹配；没有 annotation 时按 `name` 模糊匹配（包含即可）。

**输出五类差异：**

| 类型 | 判定 | 例 |
|---|---|---|
| `missing` | 期望 `required:true` 且实际次数 0 | 文档写了"点妥投确认"，实际没做 |
| `unexpected` | 实际有、期望表里没有 | 出现了一个文档没写的确认框 |
| `over_repeat` | 实际次数 > `repeat.max` | 文档说最多 21，实际点了 30 次 |
| `under_repeat` | 实际次数 < `repeat.min` | 文档说至少 1 次，实际 0 次（与 missing 合并） |
| `order` | 实际序列与期望序列的编辑距离 > 阈值（默认 3） | 先点打印再勾全选（文档写反了） |

**`order` 的算法**：把两边的 `(step, no)` 序列做 LCS，LCS 长度 / 期望长度 < 0.8 时判整体顺序异常，并列出被跳过的位置。

### 8.4 `SOP-DIFF.md`

```markdown
# SOP 一致性校验

比对对象：`自动退货处理.doc`（21 个动作）
本次记录：33 次操作，其中 32 次无人工标注

## 结论

**文档与实际流程不一致，有 4 处需要修订。**

## 文档有、实际没做（可能导致自动化失败）

| # | 动作 | 阶段 | 说明 |
|---|---|---|---|
| 15 | 点妥投确认 | C | 未记录到该操作。若真机上确实需要，文档正确、本次采集缺失；若不需要，文档应删除 |

## 实际有、文档没写（文档漏项）

| seq | 实际动作 | 阶段 | 建议补充到文档 |
|---|---|---|---|
| 101 | 点击「确定」关闭提示框 | C | 文档未提及 C 段有确认框 |

## 次数不符

| # | 动作 | 文档 | 实际 |
|---|---|---|---|
| 2 | 点确定 | 1-21 | 24 |

## 顺序异常

LCS 覆盖率 0.71（阈值 0.8），第 9-12 步顺序与文档不符。

## 无法判定

- 32/33 个动作没有人工标注，以上结论置信度 **低**。重跑时务必按数字键标注。
```

**关键要求：**
- **标注率低于 50% 时，在文件开头用粗体写明"置信度低"**，不要假装结论可靠
- `unexpected` 的匹配容差：`name` 与期望表任一项的相似度 > 0.6 视为同一动作，不算 unexpected

### 8.5 集成

- `agentpack` 生成 `SOP-DIFF.md`，放进 L1 层（保证 agent 先看到文档可能有问题）
- `report.md` 增加一节，紧跟在"一句话结论"之后
- `probe.exe sopdiff --out <DIR>` 单独可跑（改 `plan.json` 后不必重新采集）

### 8.6 单元测试（≥5 个）

1. 期望 3 步、实际 3 步完全一致 → 无差异
2. 缺 1 步 `required:true` → `missing` 命中
3. 实际多出 1 个动作 → `unexpected` 命中
4. 次数 24 > max 21 → `over_repeat` 命中
5. 顺序颠倒 → `order` 命中且给出 LCS 覆盖率

---

## 9. 可选：引导模式（`--guide`，P2，做不完可砍）

把探针从"事后校验"变成"当场提示"：

```
[引导] 下一步 #7 勾选全选（B 阶段）
       如果实际点的是别的，我会记一条偏差。
```

- 根据 `sop_flow` 和已完成的动作推算下一步
- 每次 `click` 后比对：若与期望不符 → 控制台打 `[引导] 偏差：#7 期望勾选全选，实际点了「确定」`
- 不改任何业务行为，只提示

**这个功能的价值**：操作员当场就知道自己漏了，不用等报告。但**做不完可以砍**，`sopdiff` 的事后校验是主菜。

---

## 10. 关于「自动登录」——明确不做，理由写在这里

需求里提到"自主打开网页进行登录"。**本轮不实现，只实现「登录态复用 + 登录后自动接管」。**

### 10.1 为什么不自动登录

1. **一趟性的取证，登录失败等于白跑。** 每天跑几百次的任务，失败重试即可；这个不行。
2. 内网登录普遍带 SSO 跳转、验证码、UKey/证书，自动化覆盖不全。
3. 密码必须进配置文件明文 → 又多一个凭据泄漏面。

### 10.2 实际做法

- **`--reuse-session`（默认开）**：启动时先探测登录态——若 profile 里已有有效会话（页面直接是业务页、没有跳转登录页），**跳过登录**，直接进采集。第二次及以后的运行完全不需要人操作。
- **首次运行仍需人登录一次**。操作员卡里写明：「登录一次，之后所有运行都不用再登」。
- **`--auto-login`（不实现，留接口位置）**：如果将来确实需要，只在 `values.json` 提供 `login_user`/`login_pass` 且 `--auto-login-explicit` 双重确认时才启用；任何一步不符合预期（出现验证码、跳转外域、30 秒无变化）**立即放弃并交还人工，不计为失败**，退出码 0 并在报告里记 `login: human_takeover`。

**这条写进 README，免得下一轮又提。**

---

## 11. 集成点

| 位置 | 改动 |
|---|---|
| `src/agentpack/selectors.rs` | 修复 1、3、6；新增 `looks_masked`、`element_ref`、三源合并 |
| `src/agentpack/actions.rs` | 修复 1、5；`action_uid`；`ui_delta` 改用 pointerdown DOM |
| `src/agentpack/missing.rs` | 修复 2 |
| `src/agentpack/mod.rs` | 新增 `SOP-DIFF.md` 输出，纳入 L1 |
| `src/sopdiff.rs` | **新增** |
| `src/filmstrip.rs` | 修复 4：screencast 环形缓冲 |
| `src/assets/record_click.js` | 修复 4：`pointerdown` 同步取 DOM |
| `src/app.rs` | `pointer_down` 分支；screencast 帧缓冲；`--guide` 提示 |
| `src/cli.rs` | `--guide`、`--reuse-session`；`Cmd::Sopdiff` |
| `src/report/markdown.rs` | 新增「SOP 一致性」一节 |
| `operator-card.md` | 加一句"登录一次即可"；加引导模式说明 |
| `plan.json` | 新增 `sop_flow` 块（照抄 §8.2） |

`main.rs` 的两个 `match` 都要加 `Sopdiff` 分支。

---

## 12. 禁令

1. ❌ 不要把脱敏值写进任何 selector、key、文件名。
2. ❌ 不要在 `MISSING.md` 里写解释性结论。
3. ❌ 不要为了 `ui_delta` 好看而放宽 diff 阈值。
4. ❌ 不要自动登录（§10）。
5. ❌ 不要用 `seq` 做跨会话引用。
6. ❌ 不要让 `--guide` 改变任何业务行为。
7. ❌ 不要在标注率 < 50% 时给出"文档正确"的结论——必须显式标低置信度。
8. ❌ 不要回退 README §9、§13 的任何一条已知问题记录。
9. ❌ 不要新增依赖（screencast 用现有 `cdp.call`）。

---

## 13. 验收

### 13.1 回归

- [ ] `cargo test` ≥ 78 passed / 0 failed（67 + ≥11 新增）
- [ ] 原有 26/26 H2、26/26 H1、10/10 续跑仍通过
- [ ] `cargo build --release --offline` 成功，单 exe

### 13.2 六个修复

- [ ] `SELECTORS.json` / `ACTIONS.json` 里连续 `**` 出现次数为 **0**
- [ ] `MISSING.md` 空节写「无」，全文不含「拿到了」
- [ ] `#printNos`、`#phoneInput` 出现在 `SELECTORS.json`，`sources` 含 `change` 或 `control_map`
- [ ] 一次 mock 会话：`next_waits_for.type=="none"` 占比 < 20%
- [ ] 连跑两次 `agentpack`，`action_uid` 集合完全一致
- [ ] `SELECTORS.json` 无 `on`/`off` 类 key，且原控件未丢失

### 13.3 SOP 校验

- [ ] mock 故意漏做"点妥投确认"，`SOP-DIFF.md` 的 `missing` 命中它
- [ ] mock 故意多点 24 次"确定"，`over_repeat` 命中
- [ ] 标注率 < 50% 时文件开头出现「置信度低」
- [ ] 改 `plan.json` 后 `probe sopdiff` 单独重跑，结论随之变化（证明配置真生效）

### 13.4 真 agent 二次 trial

- [ ] 新包交给 agent，**不给额外说明**，检查三点：
  - 生成的脚本里有没有含 `*` 的 selector（应为 0）
  - 有没有引用 `seq` 而不是 `action_uid`（应为 0）
  - 对 `SOP-DIFF.md` 里的 `missing` 项，是否在脚本里留了 TODO 而不是编造
- [ ] 把 agent 这次的备注合并进 README §13

---

## 14. 实施顺序

| # | 内容 | 验收 |
|---|---|---|
| 1 | 修复 1（`looks_masked` + `element_ref`） | 3 个单测 + 星号计数为 0 |
| 2 | 修复 2（`MISSING.md`） | 2 个单测 |
| 3 | 修复 5、6（`action_uid`、标签过滤） | 各 1-2 个单测 |
| 4 | 修复 3（三源合并 + 输入框字段） | `#phoneInput` 进表 |
| 5 | 修复 4（screencast + pointerdown） | `none` 占比 < 20% |
| 6 | `sopdiff.rs`（5 个单测） | 五类差异都能命中 |
| 7 | 集成到报告与 `agentpack` L1 | 十三节齐 |
| 8 | （可选）`--guide` | 控制台能提示 |
| 9 | 二次 agent trial | §13.4 |
| 10 | 重打 release + 更新 README | — |

---

## 15. 一句话

这轮做完，工具应该能回答两个不同的问题：

1. **「系统的每一个请求、每一次点击、每一份源码，我都拿到了吗？」**（上一轮已达成）
2. **「我们的 Word 文档，跟系统实际行为一致吗？」**（本轮新增）

第二个问题才是你真正要的。只回答第一个，你会得到一个精确到毫秒的错误流程。
