#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""内网寄递业务平台 · 退货流程（A/B/C/D 四阶段）自动化脚本骨架。

本文件由 AGENT_PACK3 语料编写，**只依据包内已采集证据**：
  ACTIONS.json（37 个动作：定位器 / 请求 / 界面变化 / next_waits_for）
  SELECTORS.json（去重定位器 + 稳定性）
  API.md（8 个可回放接口）· MISSING.md（缺口）· SOP-DIFF.md（文档差异）
  PAGES/*.html（页面结构）· RAW/dialogs.json（JS 对话框）· RAW/experiment21.json（单号循环结论）

设计原则（与 00-AGENT-READ-ME.md 一致）：
  1. 动作一律用 action_uid 引用，seq 只写进日志、不参与任何判断。
  2. 点击/填值用到的 selector 全部来自 ACTIONS.json 的 selector.primary，
     并在 --selfcheck 里逐个核对确实能在 SELECTORS.json 中找到（primary 或 backup）。
     脚本里不手写任何 selector 字面量。
  3. 每个动作执行后严格按该动作的 next_waits_for 等待（type=element / request 两种）。
  4. 未采到 / 存在差异的地方不猜：留 TODO，并在 NOTES.md 对应小节说明。
  5. 真实单号、手机号、打印模板在包里是脱敏的（RAW/inputs.json 只有 hash8），
     必须由 --data 提供；脚本检测到脱敏值会直接拒绝运行。

  关于代码里的 "#xxx"：`A:~input#mailNo:1` 这类是 action_uid（其中 #mailNo/#phoneInput/#returnMailNo
  都在 SELECTORS.json 内）；注释里出现的等待元素若不在 SELECTORS.json 内，NOTES.md §2.2 有专门说明。

四个阶段的计划（PLAN_*，顺序 = ACTIONS.json 的 seq 顺序）：
  A 下段扫描  11 个动作：录入 3 个单号 → 确定(/api/scan) → 关弹窗 ×3 → 确认下段(/api/scan-confirm) → 关弹窗
  B 详情单补打 17 个动作：切 Tab → 填单号串 → 查询(/api/print-query) → 全选 → 打印 ×4 → 关闭 ×6 → 手机号 → 确定(/api/print-phone)
  C 退回寄件人  7 个动作：切 Tab → 录入 → 退回寄件人(/api/return) ×2 → 妥投确认(/api/delivered) → 关闭
  D 投递归班    2 个动作：切 Tab → →(/api/handover)

运行方式（无需真机即可做的部分）：
  python automation.py --selfcheck                    # 语料一致性自检，不启浏览器
  python automation.py --dry-run --data data.json     # 演练计划，不启浏览器
  python automation.py --phase all --url http://... --data data.json --headed   # 真跑（需 playwright）

验证状态：本机已用“包内 mock 静态站 + /api 桩”冒烟跑通 37/37 步（细节见 NOTES.md §10）；
        真实内网系统未验证，9 项人工确认清单见 NOTES.md §7。

TODO 索引见 NOTES.md：§3 语义不确定、§4 定位器风险、§5 缺失动作、§6 数据与打印。
"""

from __future__ import annotations

import argparse
import json
import logging
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Sequence, Tuple

LOG = logging.getLogger("return-flow")

DEFAULT_PACK_DIR = Path(__file__).resolve().parents[1]

# value_set 状态的判定：checkbox/radio 看 checked，其余看 value 非空。
# 依据：ACTIONS.next_waits_for 里 state=value_set 出现在 #mailNo / #returnMailNo /
# #checkAllTop / #phoneInput 四类控件上（见 NOTES.md §2.2）。
VALUE_SET_JS = """(sel) => {
  const el = document.querySelector(sel);
  if (!el) return false;
  const t = (el.type || '').toLowerCase();
  if (t === 'checkbox' || t === 'radio') return !!el.checked;
  return String(el.value || '').length > 0;
}"""


# =============================================================== 1. 数据模型


@dataclass(frozen=True)
class Wait:
    """一个动作执行后要满足的等待条件，逐字段对应 ACTIONS.json 的 next_waits_for。"""

    type: str  # "element" | "request"
    selector: Optional[str] = None
    state: Optional[str] = None  # element: visible / hidden / attached / value_set
    method: Optional[str] = None  # request
    path: Optional[str] = None  # request
    timeout_ms: int = 5000

    def describe(self) -> str:
        if self.type == "request":
            return f"request {self.method} {self.path} ({self.timeout_ms}ms)"
        state = self.state or "attached"
        return f"element {self.selector} [{state}] ({self.timeout_ms}ms)"

    @property
    def is_request(self) -> bool:
        return self.type == "request"


@dataclass(frozen=True)
class Action:
    """ACTIONS.json 里的一个动作。"""

    uid: str
    phase: str  # step 字段
    kind: str  # click | change
    selector: str  # selector.primary（唯一真实来源）
    backups: Tuple[str, ...]
    wait: Wait
    request: Optional[Tuple[str, str]]  # (method, path)，首次出现的请求
    name: str  # 记录时的动作名（推断名带 "~" 前缀）
    seq: int  # 仅用于日志定位，禁止作为引用键

    @property
    def label(self) -> str:
        """去掉推断前缀后的可读标签，用作非唯一定位器的文本过滤依据。"""
        return self.name.lstrip("~").strip()

    @property
    def wants_value(self) -> bool:
        return self.kind == "change"


@dataclass(frozen=True)
class SelectorRecord:
    key: str
    primary: str
    backups: Tuple[str, ...]
    unique: bool
    occurrences: int
    stability: str
    why: str


@dataclass(frozen=True)
class PlanItem:
    """计划中的一步：action_uid + 可选的数据绑定 + 选项。"""

    uid: str
    value: Optional[Tuple[str, Optional[int]]] = None  # (RunData 字段, 下标)
    optional: bool = False  # 允许“等待不到就跳过”（仅用于语义未定的关闭动作）
    note: str = ""


@dataclass
class RunData:
    """运行所需真实数据。包内数值全部脱敏，必须外部提供。"""

    scan_mail_nos: List[str]
    print_nos: str
    receiver_phone: str
    return_mail_nos: List[str]

    REQUIRED = ("scan_mail_nos", "print_nos", "receiver_phone", "return_mail_nos")

    @classmethod
    def load(cls, path: Path) -> "RunData":
        raw = json.loads(Path(path).read_text(encoding="utf-8"))
        missing = [k for k in cls.REQUIRED if k not in raw]
        if missing:
            raise SystemExit(f"[data] {path} 缺少字段：{missing}；必需字段：{list(cls.REQUIRED)}")
        return cls(
            scan_mail_nos=[str(x) for x in raw["scan_mail_nos"]],
            print_nos=str(raw["print_nos"]),
            receiver_phone=str(raw["receiver_phone"]),
            return_mail_nos=[str(x) for x in raw["return_mail_nos"]],
        )

    def resolve(self, ref: Tuple[str, Optional[int]]) -> str:
        name, idx = ref
        value = getattr(self, name)
        if idx is None:
            return str(value)
        try:
            return str(value[idx])
        except IndexError:
            raise SystemExit(f"[data] {name}[{idx}] 越界（当前长度 {len(value)}），见 NOTES.md §6.1")

    def problems(self) -> List[str]:
        """脱敏值 / 数量不符等硬性问题。"""
        out: List[str] = []
        if "*" in "".join(self.scan_mail_nos) or "*" in self.print_nos or "*" in self.receiver_phone or "*" in "".join(self.return_mail_nos):
            out.append("数据里出现 '*'：包内数值已脱敏（7000*********** 之类），不能直接回放，必须换成真实值")
        # 语料事实：A 阶段 3 个单号（3×/api/scan）、C 阶段 2 次 /api/return
        if len(self.scan_mail_nos) != 3:
            out.append(f"scan_mail_nos 长度 {len(self.scan_mail_nos)}，记录里是 3 个（A 阶段 3 次 /api/scan）")
        if len(self.return_mail_nos) != 2:
            out.append(f"return_mail_nos 长度 {len(self.return_mail_nos)}，记录里是 2 次 /api/return")
        if not re.fullmatch(r"1\d{10}", self.receiver_phone):
            out.append(f"receiver_phone='{self.receiver_phone}' 不像 11 位手机号（记录值为 130******00）")
        printed = [x for x in re.split(r"[,\s]+", self.print_nos.strip()) if x]
        if len(printed) != 3:
            out.append(f"print_nos 含 {len(printed)} 个单号，记录里 /api/print-query 带 3 个")
        return out


# =============================================================== 2. 语料加载


class Spec:
    """ACTIONS.json + SELECTORS.json 的只读视图。"""

    def __init__(self, actions: Sequence[Action], selectors: Sequence[SelectorRecord], pack_dir: Path):
        self.pack_dir = pack_dir
        self.actions = list(actions)
        self.by_uid: Dict[str, Action] = {a.uid: a for a in self.actions}
        self.selectors = list(selectors)
        self.selector_by_primary: Dict[str, SelectorRecord] = {s.primary: s for s in self.selectors}
        self.selector_by_key: Dict[str, SelectorRecord] = {s.key: s for s in self.selectors}

    @property
    def known_selectors(self) -> set:
        """SELECTORS.json 里出现过的全部定位器字符串（primary 与 backup 都算）。"""
        known = set()
        for s in self.selectors:
            known.add(s.primary)
            known.update(s.backups)
        return known

    @classmethod
    def load(cls, pack_dir: Path) -> "Spec":
        pack_dir = Path(pack_dir)
        actions_path = pack_dir / "ACTIONS.json"
        selectors_path = pack_dir / "SELECTORS.json"
        if not actions_path.is_file() or not selectors_path.is_file():
            raise SystemExit(f"[pack] 找不到语料：{actions_path} / {selectors_path}（用 --pack 指定包目录）")

        raw_actions = json.loads(actions_path.read_text(encoding="utf-8"))["actions"]
        raw_selectors = json.loads(selectors_path.read_text(encoding="utf-8"))["selectors"]

        actions: List[Action] = []
        for a in sorted(raw_actions, key=lambda x: x["seq"]):
            w = a["next_waits_for"]
            wait = Wait(
                type=w["type"],
                selector=w.get("selector"),
                state=w.get("state"),
                method=w.get("method"),
                path=w.get("path"),
                timeout_ms=int(w.get("timeout_ms_suggested") or 5000),
            )
            if w["type"] == "request" and not (wait.method and wait.path):
                raise SystemExit(f"[pack] {a['action_uid']} 的 next_waits_for 是 request 但缺 method/path")
            req = None
            if a.get("requests"):
                r = a["requests"][0]
                req = (r["method"], r["path"])
            actions.append(
                Action(
                    uid=a["action_uid"],
                    phase=a["step"],
                    kind=a["kind"],
                    selector=a["selector"]["primary"],
                    backups=tuple(a["selector"].get("backup") or ()),
                    wait=wait,
                    request=req,
                    name=a["name"],
                    seq=int(a["seq"]),
                )
            )

        selectors = [
            SelectorRecord(
                key=s["key"],
                primary=s["primary"],
                backups=tuple(s.get("backup") or ()),
                unique=bool(s["unique"]),
                occurrences=int(s.get("occurrences") or 0),
                stability=s.get("stability") or "unknown",
                why=s.get("why") or "",
            )
            for s in raw_selectors
        ]
        return cls(actions, selectors, pack_dir)


# =============================================================== 3. 四阶段计划
# 顺序与 ACTIONS.json 的 seq 严格一致；每一条都用 action_uid 引用。
# value=(字段, 下标) 表示这一步要写入的数据；optional=True 表示语义未定、允许跳过（见 NOTES）。

PLAN_A = [
    PlanItem("A:点确定:1", value=('scan_mail_nos', 0), note="名字是“点确定”(hotkey 标注)，实际是 change@#mailNo；语义见 NOTES.md §3.1"),
    PlanItem("A:~确定:1", note="POST /api/scan，只带 1 个单号（RAW/experiment21.json）"),
    PlanItem("A:~关闭:1", note="关闭 #modalClose，等待模态遮罩变 hidden（遮罩未被 SELECTORS 收录，见 NOTES §2.2）"),
    PlanItem("A:~input#mailNo:1", value=('scan_mail_nos', 1)),
    PlanItem("A:~确定:2"),
    PlanItem("A:~关闭:2"),
    PlanItem("A:~input#mailNo:2", value=('scan_mail_nos', 2)),
    PlanItem("A:~确定:3"),
    PlanItem("A:~关闭:3"),
    PlanItem("A:~确认下段:1", note="POST /api/scan-confirm（body count=3）"),
    PlanItem("A:~关闭:4"),
]

PLAN_B = [
    PlanItem("B:~详情单补打:1", note="切换 Tab；primary button.tab 不唯一 → 运行期按标签过滤（见 resolver）"),
    PlanItem("B:~710000000004140,7100:1", value=('print_nos', None)),
    PlanItem("B:~查询:1"),
    PlanItem("B:~on:1", note="#checkAllTop 勾选（type=checkbox，走 check()）"),
    PlanItem("B:~on:2", note="change 事件由 check() 触发，本步仅断言 value_set"),
    PlanItem("B:~打印:1", note="首次“打印”弹 JS alert（RAW/dialogs.json seq=1）"),
    PlanItem("B:~打印:2"),
    PlanItem("B:~打印:3"),
    PlanItem("B:~关闭:1", optional=True, note="记录里有 6 次关闭但无对应 DOM 模态；按可选步骤执行"),
    PlanItem("B:~关闭:2", optional=True),
    PlanItem("B:~关闭:3", optional=True),
    PlanItem("B:~关闭:4", optional=True),
    PlanItem("B:~关闭:5", optional=True),
    PlanItem("B:~关闭:6", optional=True),
    PlanItem("B:~打印:4", note="随后弹出收件人手机号模态（#phoneInput）"),
    PlanItem("B:~input#phoneInput:1", value=('receiver_phone', None)),
    PlanItem("B:~确定:1", note="POST /api/print-phone；primary button.btn 不唯一 → 按标签过滤"),
]

PLAN_C = [
    PlanItem("C:~退回寄件人:1", note="切换 Tab（不是按钮 #btnReturn）"),
    PlanItem("C:~input#returnMailNo:1", value=('return_mail_nos', 0)),
    PlanItem("C:~退回寄件人:2", note="POST /api/return"),
    PlanItem("C:~input#returnMailNo:2", value=('return_mail_nos', 1)),
    PlanItem("C:~退回寄件人:3"),
    PlanItem("C:~妥投确认:1", note="POST /api/delivered；随后 JS confirm 对话框"),
    PlanItem("C:~关闭:1", optional=True, note="primary button.btn 不唯一；模态页脚“关闭”"),
]

PLAN_D = [
    PlanItem("D:~投递归班交接:1", note="切换 Tab"),
    PlanItem("D:~→:1", note="POST /api/handover，ts 为动态字段（API.md）"),
]

PLAN: Dict[str, List[PlanItem]] = {"A": PLAN_A, "B": PLAN_B, "C": PLAN_C, "D": PLAN_D}

PHASE_TITLES = {
    "A": "下段扫描",
    "B": "详情单补打",
    "C": "退回寄件人",
    "D": "投递归班交接",
}

# SOP-DIFF.md 里“文档有、实际没做/未记录”的 10 条，逐条给出当前脚本的处理方式。
# status: covered=有等价动作 / ambiguous=有记录但语义冲突 / missing=完全没有 → 必须人工补
DOC_STEPS: Tuple[Tuple[str, int, str, str, str], ...] = (
    ("A", 1, "输入邮件号", "ambiguous", "记录里只有 change@#mailNo（uid A:点确定:1，名字却是“点确定”），SOP-DIFF 又说“未记录到”→ NOTES §3.1"),
    ("B", 5, "粘贴单号串", "covered", "uid B:~710000000004140,7100:1（#printNos 一次填入逗号串）"),
    ("B", 7, "勾选全选", "covered", "uid B:~on:1 / B:~on:2（#checkAllTop）"),
    ("B", 9, "点打印", "covered", "uid B:~打印:1..4（#btnPrint）；但页面另有“确定打印”按钮未被记录 → NOTES §5.2"),
    ("C", 14, "逐单输入Enter", "missing", "记录里只有 fill，没有任何 Enter 键事件；脚本按“点退回寄件人”推进 → NOTES §3.3"),
    ("D", 17, "打开投递归班", "covered", "uid D:~投递归班交接:1"),
    ("D", 18, "选投递员", "missing", "无动作/无定位器/页面精简版也无该控件 → NOTES §5.1"),
    ("D", 19, "点查询", "missing", "同上"),
    ("D", 20, "点确定", "missing", "同上"),
    ("D", 21, "点 ->", "covered", "uid D:~→:1（#btnHandover）"),
)


# =============================================================== 4. 运行上下文


class NeedHumanDecision(RuntimeError):
    """定位器歧义且无法安全消歧时抛出，要求人工确认后再跑。"""


class StepFailed(RuntimeError):
    pass


# 记录里的“等待条件”有一处明显错位（见 NOTES §2.2）：B 阶段的输入动作等的是 td，
# 而 td 是下一步“查询”之后才渲染的表格行。默认严格按 next_waits_for 等；
# 只有在记录等待失败后，才退到这里的“恢复等待”（不改变记录本身的语义）。
# --strict-waits 可关闭恢复，便于把记录的行为差异暴露出来。
WAIT_RECOVERY: Dict[str, Wait] = {
    "B:~710000000004140,7100:1": Wait("element", "#printNos", "value_set", timeout_ms=3000),
}


class DryRunContext:
    """不启浏览器的演练上下文：完整走一遍 PLAN，打印每个动作的定位器与等待。"""

    def __init__(self, spec: Spec, data: Optional[RunData]):
        self.spec = spec
        self.data = data
        self.phase = "?"

    def start_phase(self, phase: str) -> None:
        self.phase = phase
        LOG.info("=== 阶段 %s：%s（%d 个动作）===", phase, PHASE_TITLES[phase], len(PLAN[phase]))

    def perform(self, item: PlanItem) -> None:
        action = self.spec.by_uid[item.uid]
        value = ""
        if item.value is not None:
            value = self.data.resolve(item.value) if self.data else "<需要 --data 提供 %s[%s]>" % item.value
        LOG.info(
            "[dry] %-28s kind=%-6s sel=%-14s wait=%-42s value=%s%s",
            action.uid,
            action.kind,
            action.selector,
            action.wait.describe(),
            value or "-",
            "  (optional)" if item.optional else "",
        )


class BrowserContext:
    """真实运行上下文：Playwright 页面 + 等待 + 请求校验 + 对话框处理。"""

    def __init__(self, page: Any, spec: Spec, data: RunData, click_timeout_ms: int = 5000, strict_waits: bool = False):
        self.page = page
        self.spec = spec
        self.data = data
        self.click_timeout_ms = click_timeout_ms
        self.strict_waits = strict_waits
        self.phase = "?"
        self._responses: List[Any] = []
        self._dialog_log: List[Dict[str, str]] = []
        page.on("response", self._on_response)
        page.on("dialog", self._on_dialog)

    # -- 事件 ---------------------------------------------------------------

    def _on_response(self, resp: Any) -> None:
        self._responses.append(resp)

    def _on_dialog(self, dialog: Any) -> None:
        """JS alert/confirm 处理。

        证据：RAW/dialogs.json 记录了两类原生对话框
          seq 87  “打印”      → alert  “打印任务已提交（模拟）”
          seq 151 “妥投确认”  → confirm“确认将选中记录标记为妥投？”
        处理策略：alert 只能 accept；confirm 默认 accept（因为 seq 151 之后确实产生了
        /api/delivered 请求，说明操作员点了“是”），但**包内没有记录确实点了哪个按钮**，
        见 NOTES.md §3.4。
        """
        kind = dialog.type
        policy = {"alert": "accept", "confirm": "accept", "beforeunload": "dismiss", "prompt": "dismiss"}
        action = policy.get(kind, "dismiss")
        self._dialog_log.append({"type": kind, "message": dialog.message, "handled": action})
        LOG.warning("DIALOG type=%s msg=%r → %s（策略见 NOTES §3.4）", kind, dialog.message, action)
        try:
            dialog.accept() if action == "accept" else dialog.dismiss()
        except Exception as exc:  # 对话框可能已被其它路径关闭
            LOG.warning("DIALOG 处理失败：%s", exc)

    # -- 顶层执行 ------------------------------------------------------------

    def start_phase(self, phase: str) -> None:
        self.phase = phase
        LOG.info("=== 阶段 %s：%s（%d 个动作）===", phase, PHASE_TITLES[phase], len(PLAN[phase]))

    def perform(self, item: PlanItem) -> None:
        action = self.spec.by_uid[item.uid]
        LOG.info("STEP %s (seq=%s kind=%s name=%r)", action.uid, action.seq, action.kind, action.name)
        if item.note:
            LOG.info("      note: %s", item.note)
        mark = len(self._responses)

        if action.kind == "click":
            self._do_click(action, item)
        elif action.kind == "change":
            if item.value is not None:
                self._do_type(action, item)
            else:
                # 例：B:~on:2 —— change 事件由上一步 check() 触发，本步只等断言。
                LOG.info("      断言型 change（无输入值），仅按 next_waits_for 等待")
        else:
            raise StepFailed(f"{action.uid} 未知 kind={action.kind}")

        self._satisfy_wait_with_recovery(action, item)
        self._verify_request(action, mark, item)

    def _satisfy_wait_with_recovery(self, action: Action, item: PlanItem) -> None:
        try:
            self._satisfy_wait(action.wait, optional=item.optional, action=action)
            return
        except StepFailed as exc:
            recovery = WAIT_RECOVERY.get(action.uid)
            if recovery is None or self.strict_waits:
                raise
            LOG.warning("      WAIT-RECOVERY %s：记录等待失败（%s）→ 改用恢复等待 %s（见 NOTES §2.2）",
                        action.uid, exc, recovery.describe())
        self._satisfy_wait(recovery, optional=False, action=action)

    # -- 点击 ---------------------------------------------------------------

    def _do_click(self, action: Action, item: PlanItem) -> None:
        loc = self._locate(action, item)
        try:
            loc.wait_for(state="visible", timeout=self.click_timeout_ms)
        except Exception:
            if item.optional:
                LOG.warning("      SKIP(optional)：%s 在 %dms 内不可见", action.selector, self.click_timeout_ms)
                raise _Skipped()
            raise StepFailed(f"{action.uid}: {action.selector} 不可见（{self.click_timeout_ms}ms）")

        checkbox = loc.evaluate(
            "e => e.tagName.toLowerCase() === 'input' && ['checkbox','radio'].includes((e.type||'').toLowerCase())"
        )
        if action.wait.is_request:
            with self.page.expect_response(self._request_pred(action.wait), timeout=action.wait.timeout_ms) as info:
                loc.check(timeout=self.click_timeout_ms) if checkbox else loc.click(timeout=self.click_timeout_ms)
            self._log_response(info.value, action)
            return
        loc.check(timeout=self.click_timeout_ms) if checkbox else loc.click(timeout=self.click_timeout_ms)

    # -- 填值 ---------------------------------------------------------------

    def _do_type(self, action: Action, item: PlanItem) -> None:
        value = self.data.resolve(item.value) if item.value is not None else ""
        if not value:
            raise StepFailed(f"{action.uid}: 需要输入值但数据为空（见 NOTES §6.1）")
        loc = self._locate(action, item)
        loc.wait_for(state="visible", timeout=self.click_timeout_ms)
        loc.fill("")
        loc.fill(value)  # fill 会派发 input + change，与记录到的 change 事件一致
        LOG.info("      fill %s ← %d 字符", action.selector, len(value))

    # -- 定位器解析 ----------------------------------------------------------

    def _locate(self, action: Action, item: PlanItem) -> Any:
        """按 ACTIONS.selector.primary 定位；非唯一时按动作名标签消歧；缺失时依次试 backup。"""
        loc = self.page.locator(action.selector)
        count = loc.count()

        if count == 0:
            for alt in action.backups:
                if alt == action.selector:
                    continue
                cand = self.page.locator(alt)
                if cand.count() == 1:
                    LOG.warning("      FALLBACK %s: %s → %s（backup 命中，稳定性下降，见 NOTES §4）", action.uid, action.selector, alt)
                    return cand
            raise StepFailed(f"{action.uid}: {action.selector} 匹配 0 个元素，backup 也没命中")

        if count == 1:
            return loc

        label = action.label
        if label:
            picked = self._disambiguate(loc, label)
            if picked is not None:
                LOG.warning(
                    "      AMBIGUOUS %s: %s 匹配 %d 个 → 按可见+标签 %r 收敛到 1 个（非唯一定位器，见 NOTES §4）",
                    action.uid, action.selector, count, label,
                )
                return picked
            raise NeedHumanDecision(
                f"{action.uid}: {action.selector} 匹配 {count} 个，按可见性+标签 {label!r} 收敛不到唯一元素；"
                f"请人工确认区分特征后补进 SELECTORS（MISSING.md “建议补采”）"
            )
        raise NeedHumanDecision(f"{action.uid}: {action.selector} 匹配 {count} 个且无可用标签，需人工消歧")

    @staticmethod
    def _disambiguate(loc: Any, label: str) -> Optional[Any]:
        """非唯一定位器消歧：优先“可见 + 文本完全等于动作名”，其次“可见 + 文本包含动作名”。

        只做收敛，不做猜测：收敛不到唯一元素时返回 None，由调用方要求人工决策。
        """
        exact: List[Any] = []
        contains: List[Any] = []
        for i in range(loc.count()):
            el = loc.nth(i)
            try:
                if not el.is_visible():
                    continue
                text = (el.inner_text() or "").strip()
            except Exception:
                continue
            if text == label:
                exact.append(el)
            elif label in text:
                contains.append(el)
        pool = exact or contains
        return pool[0] if len(pool) == 1 else None

    # -- 等待 ---------------------------------------------------------------

    def _satisfy_wait(self, wait: Wait, optional: bool, action: Action) -> None:
        LOG.info("      wait %s", wait.describe())
        if wait.is_request:
            return  # 已在 _do_click 里用 expect_response 等待
        sel = wait.selector
        assert sel, f"{action.uid}: element 等待缺 selector"

        # 记录里有一类“等待目标就是本次点击的元素本身，且状态是 visible”（如 A:~关闭:2 等 #modalClose visible）。
        # 点击“关闭”之后它必然不可见，按字面执行等于自相矛盾；点击前的可见性断言已经满足该条件。
        # 处理：视为前置条件，不重复等待 —— 见 NOTES §2.4。
        if wait.state == "visible" and sel == action.selector:
            LOG.info("      wait 视为前置条件（目标=本次点击元素，点击前已断言 visible；见 NOTES §2.4）")
            return

        state = wait.state or "attached"
        try:
            if state == "attached":
                self.page.wait_for_selector(sel, state="attached", timeout=wait.timeout_ms)
            elif state in ("visible", "hidden", "detached"):
                self.page.wait_for_selector(sel, state=state, timeout=wait.timeout_ms)
            elif state == "value_set":
                self.page.wait_for_function(VALUE_SET_JS, arg=sel, timeout=wait.timeout_ms)
            else:
                raise StepFailed(f"{action.uid}: 未支持的 next_waits_for.state={state!r}（见 NOTES §2.2）")
        except Exception as exc:
            if optional:
                LOG.warning("      SKIP(optional)：等待未满足（%s）：%s", wait.describe(), exc)
                raise _Skipped()
            raise StepFailed(f"{action.uid}: 等待 {wait.describe()} 失败：{exc}")

    # -- 请求校验 -------------------------------------------------------------

    @staticmethod
    def _request_pred(wait: Wait) -> Callable[[Any], bool]:
        return lambda resp: resp.request.method == wait.method and (wait.path or "") in resp.url

    def _verify_request(self, action: Action, mark: int, item: PlanItem) -> None:
        if not action.request or action.wait.is_request:
            return  # 后者已在 expect_response 中处理
        method, path = action.request
        timeout = max(1000, min(action.wait.timeout_ms, 5000))
        deadline = timeout / 1000.0
        elapsed = 0.0
        while elapsed < deadline:
            for resp in self._responses[mark:]:
                if resp.request.method == method and path in resp.url:
                    self._log_response(resp, action)
                    return
            self.page.wait_for_timeout(100)
            elapsed += 0.1
        LOG.warning(
            "      REQUEST-MISSING %s: 期望 %s %s（记录里有，但本次未观测到）；是否阻断由人工决定，见 NOTES §5.3",
            action.uid,
            method,
            path,
        )

    @staticmethod
    def _log_response(resp: Any, action: Action) -> None:
        status = resp.status
        body: Optional[dict] = None
        try:
            body = resp.json()
        except Exception:
            body = None
        code = body.get("code") if isinstance(body, dict) else None
        LOG.info("      → %s %s status=%s code=%s", resp.request.method, resp.url.split("?")[0], status, code)
        if isinstance(body, dict) and code not in (0, None):
            LOG.warning("      code=%s 非 0（样本一律 code=0/msg=成功，见 API.md；判定口径待确认 NOTES §2.3）", code)


class _Skipped(Exception):
    """内部信号：可选步骤未满足，继续后续步骤。"""


# =============================================================== 5. 自检


def _all_plan_items() -> Iterable[Tuple[str, PlanItem]]:
    for phase, items in PLAN.items():
        for item in items:
            yield phase, item


def iter_plan_uids() -> List[str]:
    return [item.uid for _, item in _all_plan_items()]


def selfcheck(spec: Spec, data: Optional[RunData] = None) -> List[Tuple[str, str]]:
    """返回 [(level, message)]；level ∈ OK / WARN / FAIL / INFO。"""
    report: List[Tuple[str, str]] = []
    pack = spec.pack_dir
    report.append(("INFO", f"pack = {pack}"))
    report.append(("INFO", f"ACTIONS.json 动作数 = {len(spec.actions)}；PLAN 覆盖 = {len(iter_plan_uids())}"))

    # 1) 覆盖与唯一性
    spec_uids = [a.uid for a in spec.actions]
    plan_uids = iter_plan_uids()
    dups = {u for u in plan_uids if plan_uids.count(u) > 1}
    missing = [u for u in spec_uids if u not in plan_uids]
    extra = [u for u in plan_uids if u not in spec.by_uid]
    if dups:
        report.append(("FAIL", f"PLAN 里重复引用的 action_uid：{sorted(dups)}"))
    if missing:
        report.append(("FAIL", f"PLAN 未覆盖的 action_uid：{missing}"))
    if extra:
        report.append(("FAIL", f"PLAN 引用了 ACTIONS.json 里不存在的 action_uid：{extra}"))

    # 2) 每个阶段内部顺序必须与 seq 一致
    for phase, items in PLAN.items():
        seqs = [spec.by_uid[i.uid].seq for i in items if i.uid in spec.by_uid]
        if seqs != sorted(seqs):
            report.append(("WARN", f"阶段 {phase} 的 PLAN 顺序与 seq 不一致：{seqs}"))

    # 3) 每个动作的 selector 必须能在 SELECTORS.json 里找到
    only_backup, unknown = [], []
    for a in spec.actions:
        if a.selector in spec.selector_by_primary:
            continue
        if a.selector in spec.known_selectors:
            only_backup.append(f"{a.uid}:{a.selector}")
        else:
            unknown.append(f"{a.uid}:{a.selector}")
    if unknown:
        report.append(("FAIL", f"定位器在 SELECTORS.json 里完全找不到：{unknown}"))
    if only_backup:
        report.append(("WARN", f"只在 SELECTORS.json 的 backup 里出现（非 primary，非唯一）：{only_backup}"))

    # 4) 非唯一定位器清单（运行期靠标签过滤）
    non_unique = [
        f"{a.uid}:{a.selector}"
        for a in spec.actions
        if (rec := spec.selector_by_primary.get(a.selector)) and not rec.unique
    ]
    if non_unique:
        report.append(("WARN", f"unique=false 的定位器（运行期按动作名标签过滤）：{non_unique}"))

    # 5) next_waits_for 用到的元素，是否被 SELECTORS.json 收录
    wait_sels = sorted({a.wait.selector for a in spec.actions if a.wait.type == "element" and a.wait.selector})
    unrated = [s for s in wait_sels if s not in spec.known_selectors]
    if unrated:
        report.append(("WARN", f"next_waits_for 里的等待元素未被 SELECTORS.json 收录（稳定性未知）：{unrated}"))

    # 6) 等待状态与请求等待清单
    states = sorted({a.wait.state or "attached" for a in spec.actions if a.wait.type == "element"})
    report.append(("INFO", f"element 等待状态集合 = {states}；request 等待动作 = "
                           f"{[a.uid for a in spec.actions if a.wait.is_request]}"))

    # 7) 有记录请求的动作
    req_actions = [(a.uid, a.request) for a in spec.actions if a.request]
    report.append(("INFO", f"带请求的动作 {len(req_actions)} 个：{[(u, f'{m} {p}') for u, (m, p) in req_actions]}"))

    # 8) 数据
    if data is None:
        report.append(("INFO", "未提供 --data：真实单号/手机号未加载（包内值已脱敏）"))
    else:
        for problem in data.problems():
            report.append(("WARN", f"数据：{problem}"))
        report.append(("INFO", f"数据：scan={len(data.scan_mail_nos)} print={data.print_nos} phone={data.receiver_phone[:3]}*** return={len(data.return_mail_nos)}"))

    # 9) 等待恢复表
    for uid, wait in WAIT_RECOVERY.items():
        report.append(("WARN", f"WAIT_RECOVERY {uid}：记录等待失败后改用 {wait.describe()}（--strict-waits 可关闭）"))

    # 10) SOP-DIFF 文档步骤对照
    for phase, no, name, status, detail in DOC_STEPS:
        level = "INFO" if status == "covered" else "WARN"
        report.append((level, f"SOP#{no:<2} [{phase}] {name}：{status} — {detail}"))

    # 11) 运行依赖
    try:
        import playwright  # noqa: F401

        report.append(("INFO", "playwright 已安装"))
    except ImportError:
        report.append(("WARN", "playwright 未安装：只能跑 --selfcheck / --dry-run；真跑前 pip install playwright && playwright install msedge"))
    return report


def print_report(report: Sequence[Tuple[str, str]]) -> int:
    order = {"FAIL": 0, "WARN": 1, "INFO": 2, "OK": 3}
    for level, msg in sorted(report, key=lambda x: order.get(x[0], 9)):
        print(f"[{level}] {msg}")
    fails = sum(1 for level, _ in report if level == "FAIL")
    warns = sum(1 for level, _ in report if level == "WARN")
    print(f"\n合计：FAIL {fails} / WARN {warns} / 共 {len(report)} 条")
    return 1 if fails else 0


# =============================================================== 6. 主流程


def run_plan(phase: str, ctx: Any, strict: bool = False) -> None:
    ctx.start_phase(phase)
    for item in PLAN[phase]:
        try:
            ctx.perform(item)
        except _Skipped:
            if strict:
                raise
            LOG.warning("跳过可选动作 %s（未满足等待；语义待确认，见 NOTES §3.2）", item.uid)


def run_phases(phases: Sequence[str], ctx: Any, strict: bool = False) -> None:
    for phase in phases:
        run_plan(phase, ctx, strict=strict)


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="内网寄递业务平台 · 退货流程四阶段自动化骨架")
    parser.add_argument("--pack", type=Path, default=DEFAULT_PACK_DIR, help="语料包目录（含 ACTIONS.json / SELECTORS.json）")
    parser.add_argument("--data", type=Path, default=None, help="真实数据 JSON（见 NOTES §6.1）")
    parser.add_argument("--selfcheck", action="store_true", help="只做语料一致性自检，不启浏览器")
    parser.add_argument("--dry-run", action="store_true", help="不启浏览器，演练计划")
    parser.add_argument("--phase", choices=["A", "B", "C", "D", "all"], default="all")
    parser.add_argument("--url", default=None, help="系统地址（包里没有记录真实地址）")
    parser.add_argument("--channel", default="msedge", help="Playwright 浏览器通道（记录用的是 Edge）")
    parser.add_argument("--headed", action="store_true", help="有头模式（打印相关步骤必须）")
    parser.add_argument("--strict", action="store_true", help="可选动作未满足时直接失败")
    parser.add_argument("--strict-waits", action="store_true", help="关闭 WAIT_RECOVERY，严格只按 next_waits_for 等待")
    parser.add_argument("--click-timeout-ms", type=int, default=5000)
    args = parser.parse_args(argv)

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)-5s %(message)s", datefmt="%H:%M:%S")

    spec = Spec.load(args.pack)
    data = RunData.load(args.data) if args.data else None
    report = selfcheck(spec, data)

    if args.selfcheck or (not args.dry_run and not args.url):
        rc = print_report(report)
        if not args.selfcheck and not args.dry_run:
            print("\n未给 --url：包里没有真实地址（RAW/report.json 只记录了本地 mock http://127.0.0.1:8080）。")
        return rc

    if args.dry_run:
        for level, msg in report:
            if level in ("FAIL", "WARN"):
                print(f"[{level}] {msg}")
        phases = list("ABCD") if args.phase == "all" else [args.phase]
        run_phases(phases, DryRunContext(spec, data))
        return 0 if not any(level == "FAIL" for level, _ in report) else 1

    # ---- 真跑 ----
    if data is None:
        print("[FAIL] 真跑必须提供 --data（包内单号/手机号已脱敏）")
        return 1
    problems = data.problems()
    if problems:
        for problem in problems:
            print(f"[FAIL] 数据：{problem}")
        return 1
    try:
        from playwright.sync_api import sync_playwright  # 延迟导入：自检/演练不需要
    except ImportError:
        print("[FAIL] 缺少 playwright：pip install playwright && playwright install msedge")
        return 1

    phases = list("ABCD") if args.phase == "all" else [args.phase]
    with sync_playwright() as pw:
        browser = pw.chromium.launch(channel=args.channel, headless=not args.headed)
        page = browser.new_page()
        ctx = BrowserContext(page, spec, data, click_timeout_ms=args.click_timeout_ms, strict_waits=args.strict_waits)
        try:
            page.goto(args.url, wait_until="domcontentloaded")
            run_phases(phases, ctx, strict=args.strict)
        except NeedHumanDecision as exc:
            LOG.error("需要人工决策：%s", exc)
            return 2
        except StepFailed as exc:
            LOG.error("步骤失败：%s", exc)
            return 1
        finally:
            if ctx._dialog_log:
                LOG.info("本轮自动处理的对话框：%s", ctx._dialog_log)
            browser.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
