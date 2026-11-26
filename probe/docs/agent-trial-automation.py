#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""内网寄递业务平台 · 退货流程四阶段自动化骨架（Python + Playwright，同步 API）。

覆盖 00-AGENT-READ-ME.md 指定的 SOP：
    A 下段扫描      每单「输入 → 确定 → ×」×3 → 确认下段 → ×
    B 详情单补打    页签 → 查询 → 全选 → 打印 ×4（3 个分支）→ 手机号弹窗 → 确定
    C 退回寄件人    页签 → 退回 ×2 → 妥投确认 → 关闭
    D 投递归班交接  页签 → →

只读依赖（不改包）：
    ACTIONS.json   33 个动作：seq / 定位器 / 请求 / ui_delta / next_waits_for
    SELECTORS.json 16 条去重定位器（含 5 条 unique:false）
    API.md         9 个 POST /api/* 的可回放性判定
    MISSING.md     未采到的内容（一律留 TODO，不臆造）
    RAW/mirror/    页面源码（只用于解释行为分支与 js 弹窗）

设计要点
--------
1. 定位器：全部来自 SELECTORS.json 的 primary/backup，代码里用 key 标注；
   启动时 verify_pack() 逐条对账（key 集合、primary/backup/unique/stability、
   记录里的 primary 是否属于该 key 的候选集），不一致直接退出。
2. seq 不写死：包在采集端重新生成时 seq 会整体重编号（本次实测 6 分钟内变了两次），
   所以脚本按「阶段内顺序」把声明式流程（SPECS）与 ACTIONS.json 对齐，
   seq / no / name / 等待条件都在 import 时从包里解析出来（build_actions()）。
3. 等待条件：类型（element/request/none）与目标、请求路径**完全来自** next_waits_for；
   只有「元素等待的方向（可见/隐藏）」由脚本按动作语义决定，因为 ui_delta 记录的
   可见性在 A 阶段前后颠倒（NOTES §4 有逐条对照）。方向差异会被 --selfcheck 列出来。
4. MISSING.md 与「包里没登记」的控件在调用点 gap() → TODO(pack-gap)，--strict 时直接失败。

用法
----
    python automation.py --selfcheck               # 只校验脚本 ↔ 包，不启动浏览器
    python automation.py --dry-run                 # 打印计划（含解析出的 seq）
    python automation.py --base-url http://<内网地址> --numbers-file nos.txt --headed --strict
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from contextlib import ExitStack
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

# 包目录：默认本脚本上一级（OUT/ 的父目录），可用环境变量覆盖
PACK_DIR = Path(os.environ.get("AGENT_PACK_DIR") or Path(__file__).resolve().parent.parent)

# 页面地址：包里没有记录真实内网地址（NOTES §8），只能外部传入
DEFAULT_BASE_URL = os.environ.get("PLATFORM_URL", "http://127.0.0.1:8080")


# ════════════════════════════════════════════════════════════════════════════
# 1. 运行期配置与日志
# ════════════════════════════════════════════════════════════════════════════
@dataclass
class Config:
    base_url: str = DEFAULT_BASE_URL
    headed: bool = False
    strict: bool = False             # True: 触达 pack-gap 直接失败（实机推荐）
    none_settle_ms: int = 400        # type=none 的静默窗口
    resolve_timeout_ms: int = 5000   # 定位器轮询上限
    slow_mo_ms: int = 0
    screenshot: bool = True
    artifact_dir: Path | None = None
    numbers: list[str] = field(default_factory=list)


CFG = Config()


def log(msg: str) -> None:
    print(f"[{datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)


def mask_mail_no(value: str) -> str:
    """日志不落单号原文（包里的值本来也是脱敏的）。"""
    v = (value or "").strip()
    return v if len(v) <= 6 else f"{v[:5]}***{v[-2:]}"


class PackError(RuntimeError):
    """脚本与包不一致 / 定位失败 / 接口返回异常。"""


class PackGap(PackError):
    """触达 MISSING.md 或「包里没有定位器」的缺口。"""


# ════════════════════════════════════════════════════════════════════════════
# 2. 控件定位器（逐条对应 SELECTORS.json，注释里的 key 就是 JSON 的 key）
# ════════════════════════════════════════════════════════════════════════════
@dataclass(frozen=True)
class Sel:
    """一条 SELECTORS.json 记录：primary/backup 原样搬运，不做“优化”。"""

    key: str
    primary: str
    backup: tuple[str, ...]
    unique: bool
    stability: str


SEL: dict[str, Sel] = {
    # ── 阶段 A ────────────────────────────────────────────────────────────
    # key="A.点确定"          #mailNo；seq=24（记录标成 change）
    "A.点确定": Sel("A.点确定", "#mailNo",
                    ("#mailNo", "//input[normalize-space()='7100***********']"), True, "high"),
    # key="A.7100***********" #mailNo；seq=33/42
    "A.7100***********": Sel("A.7100***********", "#mailNo",
                             ("#mailNo", "//input[normalize-space()='7100***********']"), True, "high"),
    # key="A.确定"            #btnOk
    "A.确定": Sel("A.确定", "#btnOk",
                  ("#btnOk", "button.btn", "//button[normalize-space()='确定']"), True, "high"),
    # key="A.×"              #modalClose
    "A.×": Sel("A.×", "#modalClose",
               ("#modalClose", "button.modal-close", "//button[normalize-space()='×']"), True, "high"),
    # key="A.确认下段"         #confirmScan
    "A.确认下段": Sel("A.确认下段", "#confirmScan",
                    ("#confirmScan", "button.btn", "//button[normalize-space()='确认下段']"), True, "high"),
    # ── 阶段 B ────────────────────────────────────────────────────────────
    # key="B.详情单补打"       unique:false（无 id，靠 class）
    "B.详情单补打": Sel("B.详情单补打", "button.tab", (
        "html > body:nth-child(2) > div:nth-child(1) > nav:nth-child(2) > button:nth-child(2)",
        "button.tab",
        "//button[normalize-space()='详情单补打']",
    ), False, "low"),
    # key="B.查询"            #btnQuery
    "B.查询": Sel("B.查询", "#btnQuery",
                ("#btnQuery", "button.btn", "//button[normalize-space()='查询']"), True, "high"),
    # key="B.on"              #checkAllTop（全选）
    "B.on": Sel("B.on", "#checkAllTop", ("#checkAllTop", "//input[normalize-space()='on']"), True, "high"),
    # key="B.×"              #modalClose（B 阶段用于逐条关提示）
    "B.×": Sel("B.×", "#modalClose",
               ("#modalClose", "button.modal-close", "//button[normalize-space()='×']"), True, "high"),
    # key="B.打印"            #btnPrint（4 次点击 3 个分支）
    "B.打印": Sel("B.打印", "#btnPrint",
                ("#btnPrint", "button.btn", "//button[normalize-space()='打印']"), True, "high"),
    # key="B.确定"            unique:false（弹窗页脚“确定”）
    "B.确定": Sel("B.确定", "button.btn", (
        "html > body:nth-child(2) > div:nth-child(2) > div:nth-child(1) > div:nth-child(3) > button:nth-child(1)",
        "button.btn",
        "//button[normalize-space()='确定']",
    ), False, "low"),
    # ── 阶段 C ────────────────────────────────────────────────────────────
    # key="C.退回寄件人"       unique:false；页签 seq=101，面板按钮 #btnReturn seq=102/107
    "C.退回寄件人": Sel("C.退回寄件人", "button.tab", (
        "html > body:nth-child(2) > div:nth-child(1) > nav:nth-child(2) > button:nth-child(3)",
        "button.tab",
        "//button[normalize-space()='退回寄件人']",
        "#btnReturn",
        "button.btn",
    ), False, "low"),
    # key="C.妥投确认"         #btnDelivered
    "C.妥投确认": Sel("C.妥投确认", "#btnDelivered",
                    ("#btnDelivered", "button.btn", "//button[normalize-space()='妥投确认']"), True, "high"),
    # key="C.关闭"            unique:false（弹窗页脚“关闭”）
    "C.关闭": Sel("C.关闭", "button.btn", (
        "html > body:nth-child(2) > div:nth-child(2) > div:nth-child(1) > div:nth-child(3) > button:nth-child(1)",
        "button.btn",
        "//button[normalize-space()='关闭']",
    ), False, "low"),
    # ── 阶段 D ────────────────────────────────────────────────────────────
    # key="D.投递归班交接"     unique:false
    "D.投递归班交接": Sel("D.投递归班交接", "button.tab", (
        "html > body:nth-child(2) > div:nth-child(1) > nav:nth-child(2) > button:nth-child(4)",
        "button.tab",
        "//button[normalize-space()='投递归班交接']",
    ), False, "low"),
    # key="D.→"              #btnHandover
    "D.→": Sel("D.→", "#btnHandover",
               ("#btnHandover", "button.btn", "//button[normalize-space()='→']"), True, "high"),
}


# ════════════════════════════════════════════════════════════════════════════
# 3. 等待目标（不是控件：SELECTORS.json 只登记可点击/可输入控件）
# ════════════════════════════════════════════════════════════════════════════
@dataclass(frozen=True)
class WaitTarget:
    name: str
    selector: str
    provenance: str


# 这两个 selector 抄自 ACTIONS.json 的 next_waits_for，不是脚本自创；
# verify_pack() 会校验它们确实出现在包的等待条件里（例外原因见 NOTES §1）。
WAIT_TARGETS: dict[str, WaitTarget] = {
    "modal_mask": WaitTarget("modal_mask", "#modalMask", "ACTIONS.json next_waits_for"),
    "modal_body": WaitTarget("modal_body", "#modalBody", "ACTIONS.json next_waits_for"),
}
TARGET_BY_SELECTOR: dict[str, str] = {t.selector: name for name, t in WAIT_TARGETS.items()}


# ════════════════════════════════════════════════════════════════════════════
# 4. 声明式流程（SOP 顺序），seq / 等待条件在 import 时从包里解析
# ════════════════════════════════════════════════════════════════════════════
@dataclass(frozen=True)
class Spec:
    """一条动作声明：只写“做什么”，不写 seq。

    step/kind 用于和 ACTIONS.json 的阶段内顺序对齐；
    key 指向 SELECTORS.json；prefer 必须是该 key 的 primary/backup 之一。
    """

    step: str
    kind: str                 # click | change
    key: str
    note: str = ""
    prefer: str | None = None
    also: tuple[str, str] | None = None   # 记录只给 element 等待时，附加的请求守卫


# 关于 MISSING.md 里“意图不明”的动作（本次包列了 15 条：输入单号、× 关闭、打印、
# 页签切换、勾选等）：它们的动作名/作用在包里无法确认，本脚本**不猜业务语义**，
# 只按“该元素在 SOP 位置上的必然作用”实现，并在每个对应 Spec.note 里写明依据；
# 其中 3 条真正无法判定的保留 TODO，实机必须复核：
#   - B 阶段第 6 次 ×（队列为空时是否为空操作）
#   - B 阶段第 2 次打印（源码说无弹窗，但拿不到真实平台行为）
#   - A 阶段 3 次输入后的“确定”是否需要先失焦/回车才触发校验
SPECS: tuple[Spec, ...] = (
    # ── A 下段扫描：每单 (输入 → 确定 → ×) ×3，然后 确认下段 → × ────────────
    Spec("A", "change", "A.点确定",
         note="第 1 单：往 #mailNo 输单号。记录把它标成 change（动作名“点确定”是标注偏差）。"),
    Spec("A", "click", "A.确定", note="确定 → POST /api/scan → 弹窗『邮件号码 … 下段扫描成功』。"),
    Spec("A", "click", "A.×", note="关闭弹窗。"),
    Spec("A", "change", "A.7100***********", note="第 2 单。"),
    Spec("A", "click", "A.确定", note="确定 → /api/scan。"),
    Spec("A", "click", "A.×", note="关闭弹窗。"),
    Spec("A", "change", "A.7100***********", note="第 3 单。"),
    Spec("A", "click", "A.确定", note="确定 → /api/scan。"),
    Spec("A", "click", "A.×", note="关闭弹窗。"),
    Spec("A", "click", "A.确认下段",
         note="确认下段：count 由页面按已扫描数提交（RAW/mirror/app.js:123）。",
         also=("POST", "/api/scan-confirm")),
    Spec("A", "click", "A.×", note="关闭『确认下段完成』弹窗。"),
    # ── B 详情单补打：页签 → 查询 → 全选 → 打印 ×4 → 逐条关提示 ─────────────
    Spec("B", "click", "B.详情单补打", prefer="//button[normalize-space()='详情单补打']",
         note="页签；unique:false → 按文本定位（NOTES §6）。"),
    Spec("B", "click", "B.查询", note="查询 → POST /api/print-query。"),
    Spec("B", "click", "B.on", note="全选。"),
    Spec("B", "change", "B.on", note="同一复选框的 change：点击已原生触发，不重复点击，只断言勾选态。"),
    Spec("B", "click", "B.打印", note="第 1 次打印 → 原生 alert『打印任务已提交（模拟）』，由 dialog 处理器 accept。"),
    Spec("B", "click", "B.打印", note="第 2 次打印 → 无弹窗（分支 B1）。"),
    Spec("B", "click", "B.打印", note="第 3 次打印 → 一次压入 5 条 DOM 提示（分支 B2）。"),
    Spec("B", "click", "B.×", note="逐条关闭提示 1。"),
    Spec("B", "click", "B.×", note="逐条关闭提示 2。"),
    Spec("B", "click", "B.×", note="逐条关闭提示 3。"),
    Spec("B", "click", "B.×", note="逐条关闭提示 4。"),
    Spec("B", "click", "B.×", note="逐条关闭提示 5。"),
    Spec("B", "click", "B.×", note="第 6 次 ×（记录里意图不明，队列空时为空操作）。"),
    Spec("B", "click", "B.打印", note="第 4 次打印 → 打开『填写收件人手机号』弹窗。"),
    Spec("B", "click", "B.确定", prefer="//button[normalize-space()='确定']",
         note="确定 → POST /api/print-phone；unique:false → 按文本取可见的那个“确定”。"),
    # ── C 退回寄件人 ─────────────────────────────────────────────────────
    Spec("C", "click", "C.退回寄件人", prefer="//button[normalize-space()='退回寄件人']",
         note="页签；同文本还有 #btnReturn，靠可见性判定。"),
    Spec("C", "click", "C.退回寄件人", prefer="#btnReturn",
         note="第 1 次退回 → POST /api/return。"),
    Spec("C", "click", "C.退回寄件人", prefer="#btnReturn",
         note="第 2 次退回（让 /api/delivered 的 count 凑成 2）。"),
    Spec("C", "click", "C.妥投确认",
         note="妥投确认：先弹原生 confirm，accept 后才发 /api/delivered。",
         also=("POST", "/api/delivered")),
    Spec("C", "click", "C.关闭", prefer="//button[normalize-space()='关闭']",
         note="关闭结果弹窗；unique:false。"),
    # ── D 投递归班交接 ───────────────────────────────────────────────────
    Spec("D", "click", "D.投递归班交接", prefer="//button[normalize-space()='投递归班交接']",
         note="页签；unique:false → 按文本定位。"),
    Spec("D", "click", "D.→", note="→ → POST /api/handover（ts 由页面生成，脚本不构造）。"),
)


@dataclass(frozen=True)
class Wait:
    kind: str                      # element(dom) | request | none
    target: str | None = None      # element: WAIT_TARGETS 的键
    method: str | None = None      # request
    path: str | None = None        # request
    expect: str = "visible"        # element: visible | hidden
    timeout_ms: int = 8000


W_NONE = Wait("none")


@dataclass(frozen=True)
class Act:
    no: int                 # 阶段内编号（来自包）
    seq: int                # 全局序号（来自包，随每次重采集变化）
    step: str
    kind: str
    name: str               # 包里的动作名（多为推断名）
    key: str                # SELECTORS.json key
    wait: Wait
    prefer: str | None = None
    note: str = ""
    also_request: tuple[str, str] | None = None
    recorded: str = ""      # 包里的原始等待摘要，便于对照


def load_pack(pack_dir: Path = PACK_DIR) -> tuple[dict, dict]:
    sel_path = pack_dir / "SELECTORS.json"
    act_path = pack_dir / "ACTIONS.json"
    if not sel_path.exists() or not act_path.exists():
        raise PackError(f"找不到 {sel_path} 或 {act_path}；用 AGENT_PACK_DIR 指定包目录")
    return (json.loads(sel_path.read_text(encoding="utf-8")),
            json.loads(act_path.read_text(encoding="utf-8")))


def element_direction(primary: str, target: str) -> str:
    """元素等待的方向：类型/目标来自 next_waits_for，方向由动作语义决定。

    为什么不直接用 ui_delta：A 阶段记录的可见性前后颠倒（“确定”后记成 hidden、
    “×”后记成 visible），按它写会前后矛盾（NOTES §4 有完整对照表）。
    规则只有三条：
      - 目标是 #modalBody（只在“关掉一条后看下一条”出现）→ 等可见
      - 动作是关闭按钮 #modalClose → 等隐藏
      - 其余（确定/确认下段/妥投确认/打印等结果类）→ 等可见
    """
    if target == "modal_body":
        return "visible"
    if primary == "#modalClose":
        return "hidden"
    return "visible"


def wait_from_record(primary: str, rec: dict) -> Wait:
    """把 next_waits_for 转成本脚本的 Wait —— 类型/目标/路径原样照搬。"""
    kind = (rec or {}).get("type") or "none"
    timeout = int((rec or {}).get("timeout_ms_suggested") or 0)
    if kind == "request":
        return Wait("request", method=rec.get("method"), path=rec.get("path"),
                    timeout_ms=timeout or 8000)
    if kind == "element":
        sel = rec.get("selector")
        target = TARGET_BY_SELECTOR.get(sel)
        if target is None:
            raise PackError(f"next_waits_for 的 selector {sel!r} 不在 WAIT_TARGETS 里；"
                            f"新增等待目标需要人工确认（NOTES §1）")
        return Wait("element", target=target, expect=element_direction(primary, target),
                    timeout_ms=timeout or 5000)
    return W_NONE


def build_actions(pack_dir: Path = PACK_DIR) -> tuple[tuple[Act, ...], list[str]]:
    """按「阶段内顺序」把 SPECS 与 ACTIONS.json 对齐，取出 seq/no/name/等待。"""
    _, act_json = load_pack(pack_dir)
    per_step: dict[str, list[dict]] = {}
    for j in act_json["actions"]:
        per_step.setdefault(j["step"], []).append(j)

    acts: list[Act] = []
    used = {step: 0 for step in per_step}
    for spec in SPECS:
        bucket = per_step.get(spec.step, [])
        i = used.get(spec.step, 0)
        if i >= len(bucket):
            raise PackError(f"包里 {spec.step} 阶段只有 {len(bucket)} 个动作，脚本声明了更多；"
                            f"请对照 ACTIONS.json 更新 SPECS")
        j = bucket[i]
        used[spec.step] = i + 1
        if j["kind"] != spec.kind:
            raise PackError(f"{spec.step} 阶段第 {i + 1} 个动作在包里是 {j['kind']}，"
                            f"脚本声明成 {spec.kind}（seq={j['seq']}）；请核对 SPECS")
        rec = j.get("next_waits_for") or {}
        wait = wait_from_record(j["selector"]["primary"], rec)
        rec_desc = f"{rec.get('type')}:{rec.get('selector') or rec.get('path')}"
        acts.append(Act(
            no=j["no"], seq=j["seq"], step=j["step"], kind=j["kind"], name=j["name"],
            key=spec.key, wait=wait, prefer=spec.prefer, note=spec.note,
            also_request=(spec.also if (spec.also and wait.kind == "element") else None),
            recorded=rec_desc,
        ))

    # 反过来检查：包里有没有脚本漏掉的动作（结构变了必须报错，不能静默少跑）
    leftovers = []
    for step, bucket in per_step.items():
        rest = bucket[used.get(step, 0):]
        leftovers += [f"{step} seq={x['seq']} {x['kind']} {x['selector']['primary']}" for x in rest]
    return tuple(acts), leftovers


ACTS, LEFTOVERS = build_actions()
ACT_BY_SEQ: dict[int, Act] = {a.seq: a for a in ACTS}


def acts_of(step: str) -> list[Act]:
    return [a for a in ACTS if a.step == step]


# ════════════════════════════════════════════════════════════════════════════
# 5. 包里没登记的内容：调用点留 TODO，绝不臆造
# ════════════════════════════════════════════════════════════════════════════
PACK_GAPS: dict[str, str] = {
    "print_nos": "#printNos（B 阶段“邮件号码（英文逗号分隔）”输入框）没有出现在 SELECTORS.json，"
                 "MISSING.md 也没记录这个落差；PAGES/B-详情单补打.html 里能看到该元素，但包里没有它的定位器"
                 " → 不假设、不硬编码。",
    "phone_input": "#phoneInput（B 阶段“填写收件人手机号”弹窗输入框）同样未出现在 SELECTORS.json "
                   "→ 填号一步缺失。",
    "return_mail_no": "#returnMailNo（C 阶段退回单号输入框）未出现在 SELECTORS.json，且录制里没有对应的"
                      "输入动作 → 退回步骤缺填值。",
    "status_bar": "#status（底部状态栏“请求中 /api/x”“完成 /api/x”）未登记 → 不能拿状态文本当等待条件。",
    "login": "包里没有任何登录/鉴权步骤（无登录页、无 token、无 cookie 导出）→ 脚本假定浏览器已有会话。",
    "frame": "包里没有 iframe 信息；若真实平台把表单放在 iframe 内，顶层定位会全部失效。",
    "real_url": "没有真实内网地址（RAW/mirror 目录名是 127.0.0.1_8080，属模拟站）→ 必须 --base-url 传入。",
    "panel_d": "#panel-D（“当前批次：未开始/已交接”）未登记 → D 阶段结束态无法做文本断言。",
}


def gap(name: str, detail: str | None = None) -> None:
    msg = PACK_GAPS.get(name, name)
    if detail:
        msg = f"{msg} ｜ {detail}"
    log(f"TODO(pack-gap) [{name}] {msg}")
    if CFG.strict:
        raise PackGap(f"pack-gap: {name}: {msg}")


# ════════════════════════════════════════════════════════════════════════════
# 6. 等待引擎（element / request / none 三类口径）
# ════════════════════════════════════════════════════════════════════════════
def _expect() -> Any:
    """延迟导入：--selfcheck / --dry-run 不需要安装 playwright。"""
    from playwright.sync_api import expect

    return expect


def wait_element(page: Any, wait: Wait) -> None:
    """type=element（dom）：断言目标元素“可见”或“隐藏”。

    不用 page.wait_for_selector(state=...)：同一个 #modalMask 在“打开后可见、关闭后带
    hidden 类”之间反复切换，expect 才能表达两个方向，超时信息也带实际状态。
    """
    assert wait.target is not None
    target = WAIT_TARGETS[wait.target]
    loc = page.locator(target.selector)
    expect = _expect()
    if wait.expect == "visible":
        expect(loc).to_be_visible(timeout=wait.timeout_ms)
    else:
        expect(loc).to_be_hidden(timeout=wait.timeout_ms)


def resp_pred(wait: Wait) -> Callable[[Any], bool]:
    """只认“方法一致 + URL 以 path 结尾”，避免把静态资源、带 query 的 URL 算进来。"""
    method = (wait.method or "").upper()
    suffix = re.escape(wait.path or "")

    def pred(resp: Any) -> bool:
        return (resp.request.method.upper() == method
                and re.search(suffix + r"(?:[?#]|$)", resp.url) is not None)

    return pred


def check_response(resp: Any, wait: Wait) -> dict:
    """API.md 的 9 个接口响应体都是 {code,msg,count,serverTime}：code==0 才算成功。"""
    try:
        body = resp.json()
    except Exception as exc:
        raise PackError(f"{wait.method} {wait.path}: 响应体不可解析（{exc}）；status={resp.status}") from exc
    if resp.status != 200 or body.get("code") != 0:
        raise PackError(f"{wait.method} {wait.path}: 返回异常 status={resp.status} body={body}")
    log(f"   ↳ {wait.method} {wait.path} → {resp.status} code={body.get('code')} "
        f"msg={body.get('msg')!r} count={body.get('count')}")
    return body


def settle(page: Any, why: str = "") -> None:
    """type=none：记录里没有可判定条件。

    不假装验证：只做固定静默窗口（默认 400ms），副作用留给后面带条件的等待兜住。
    为什么不拿 #status 文本当条件：它没有登记在 SELECTORS.json（NOTES §5）。
    """
    page.wait_for_timeout(CFG.none_settle_ms)
    if why:
        log(f"   ↳ 静默 {CFG.none_settle_ms}ms（{why}）")


# ════════════════════════════════════════════════════════════════════════════
# 7. 定位器解析 + 动作执行
# ════════════════════════════════════════════════════════════════════════════
def first_visible(loc: Any) -> Any | None:
    """同一表达式可能命中多个元素（4 个页签、2 个“确定”）：取第一个可见的。"""
    try:
        n = loc.count()
    except Exception:
        return None
    for i in range(n):
        cand = loc.nth(i)
        try:
            if cand.is_visible():
                return cand
        except Exception:
            continue
    return None


def candidates(act: Act) -> list[str]:
    """候选定位器顺序，全部取自该 key 的 primary/backup（脚本不自造定位器）。

    unique:true  → primary 优先，backup 兜底。
    unique:false → 文本 XPath（backup 里带 normalize-space 那条）优先：包里已说明
                   “没有 id，靠 class 定位；class 可能带构建哈希，需人工确认”。
    """
    sel = SEL[act.key]
    ordered: list[str] = []
    if act.prefer:
        ordered.append(act.prefer)
    if not sel.unique:
        ordered += [b for b in sel.backup if b.startswith("//") and "normalize-space" in b]
    ordered.append(sel.primary)
    ordered += [b for b in sel.backup if b != sel.primary]

    seen: set[str] = set()
    out: list[str] = []
    for c in ordered:
        if c and c not in seen:
            seen.add(c)
            out.append(c)
    return out


def resolve(page: Any, act: Act) -> tuple[Any, str]:
    """按候选顺序轮询到第一个可见元素；超时打印全部候选，方便实机确认。"""
    deadline = time.monotonic() + CFG.resolve_timeout_ms / 1000
    tried: set[str] = set()
    while time.monotonic() < deadline:
        for cand in candidates(act):
            tried.add(cand)
            hit = first_visible(page.locator(cand))
            if hit is not None:
                log(f"   定位 {act.key} ← {cand!r}")
                return hit, cand
        page.wait_for_timeout(120)
    sel = SEL[act.key]
    raise PackError(
        f"seq={act.seq} 定位失败（key={act.key}, unique={sel.unique}, stability={sel.stability}）；"
        f"已尝试：\n" + "\n".join(f"    - {c}" for c in sorted(tried))
    )


def capture(page: Any, act: Act) -> None:
    """每个动作后留一张图，便于和 SCREENSHOTS/seq 对齐排查。"""
    if not CFG.screenshot or CFG.artifact_dir is None:
        return
    try:
        page.screenshot(path=str(Path(CFG.artifact_dir) / f"{act.seq:06d}-{act.step}-after.png"))
    except Exception as exc:
        log(f"   截图失败（忽略）：{exc}")


def perform(page: Any, act: Act, value: str | None = None) -> None:
    """执行一条动作，并按它从包里解析出的等待条件等待。"""
    log(f"seq={act.seq} no={act.no} [{act.step}] {act.kind} {act.name!r} → key={act.key}")
    if act.note:
        log(f"   · {act.note}")

    wait = act.wait
    req: Wait | None = wait if wait.kind == "request" else None
    if req is None and act.also_request:
        req = Wait("request", method=act.also_request[0], path=act.also_request[1],
                   timeout_ms=wait.timeout_ms)

    def do() -> None:
        target, used = resolve(page, act)
        if act.kind == "click":
            target.click(timeout=10000)
        else:
            if value is None:
                raise PackError(f"seq={act.seq} 是输入动作但没有给值")
            target.fill("")
            target.fill(value)          # fill 会派发 input + change，覆盖录到的 change 事件
            log(f"   填入 {mask_mail_no(value)} (via {used!r})")

    info = None
    try:
        with ExitStack() as stack:
            if req is not None:
                info = stack.enter_context(
                    page.expect_response(resp_pred(req), timeout=req.timeout_ms)
                )
            do()

        if req is not None:
            check_response(info.value, req)

        if wait.kind == "element":
            wait_element(page, wait)
            log(f"   ↳ 等待 {WAIT_TARGETS[wait.target].selector} {wait.expect} ok")
        elif wait.kind == "none":
            settle(page, "next_waits_for.type=none")
    except PackError:
        raise
    except Exception as exc:
        if "TimeoutError" in type(exc).__name__:   # playwright 的 TimeoutError（含子类）
            raise PackError(
                f"seq={act.seq} 等待超时（{describe_wait(wait)}，包记录的是 {act.recorded}）；"
                f"若页面确实没有副作用，多半是缺少必填输入（见上面的 TODO(pack-gap)）"
                f"或该动作在实机上有额外副作用（NOTES §4/§5）"
            ) from exc
        raise

    capture(page, act)


# ════════════════════════════════════════════════════════════════════════════
# 8. 阶段 A / B / C / D
# ════════════════════════════════════════════════════════════════════════════
def modal_visible(page: Any) -> bool:
    return page.locator(WAIT_TARGETS["modal_mask"].selector).is_visible()


def modal_text(page: Any) -> str:
    try:
        return page.locator(WAIT_TARGETS["modal_mask"].selector).inner_text().strip()
    except Exception:
        return ""


def assert_modal_contains(page: Any, needle: str, timeout_ms: int = 5000) -> None:
    """弹窗文案断言：只读 #modalMask 的文本，不引入任何新定位器。"""
    expect = _expect()
    loc = page.locator(WAIT_TARGETS["modal_mask"].selector)
    try:
        expect(loc).to_contain_text(needle, timeout=timeout_ms)
    except Exception:
        raise PackError(f"弹窗文案不含 {needle!r}，实际：{modal_text(page)[:200]!r}") from None


def phase_a(page: Any, numbers: list[str]) -> None:
    """A：每单「输入 → 确定 → ×」×3，然后「确认下段 → ×」。"""
    log("═══ 阶段 A：下段扫描 ═══")
    acts = acts_of("A")
    changes = [a for a in acts if a.kind == "change"]
    if len(acts) < 3:
        raise PackError("A 阶段动作不足 3 个，包结构异常")
    triples = [acts[0:3], acts[3:6], acts[6:9]]
    tail = acts[9:]
    if len(numbers) < len(changes):
        raise PackError(f"A 阶段需要 {len(changes)} 个单号（录制了 {len(changes)} 次输入），"
                        f"实际只给了 {len(numbers)} 个")
    if len(numbers) > len(changes):
        log(f"   ! 录制只包含 {len(changes)} 个单号的循环；多出的 {len(numbers) - len(changes)} 个"
            f"复用最后一组的动作模板（无录制佐证，NOTES §7）")

    for idx, mail_no in enumerate(numbers):
        triple = triples[idx] if idx < len(triples) else triples[-1]
        fill_act, ok_act, close_act = triple
        log(f"— 第 {idx + 1}/{len(numbers)} 单：{mask_mail_no(mail_no)}（模板 seq={fill_act.seq}）")
        perform(page, fill_act, mail_no)
        perform(page, ok_act)
        assert_modal_contains(page, "下段扫描成功")
        perform(page, close_act)

    for act in tail:
        perform(page, act)
        if act.key == "A.确认下段":
            assert_modal_contains(page, "确认下段完成")


def drain_modal_queue(page: Any, run: list[Act]) -> None:
    """B2 分支：把排队的一串 DOM 提示逐个关掉。

    记录给每次 × 的等待方向并不自洽（有的写 #modalBody 可见、有的写 #modalMask 隐藏，
    而队列长度在录制与源码之间也不一致），所以这里按**实际状态**二选一：要么下一条
    出现（等 #modalBody 可见），要么队列抽干（等 #modalMask 隐藏），并把记录值写进日志。
    """
    for act in run:
        if not modal_visible(page):
            log(f"   队列已空（seq={act.seq} 关闭前遮罩不可见；记录 `{act.recorded}`）")
            return
        target, used = resolve(page, act)
        log(f"seq={act.seq} [B] 关闭提示 {modal_text(page)[:40]!r} (via {used!r})；"
            f"记录 `{act.recorded}`，按实际状态自适应")
        target.click(timeout=10000)
        page.wait_for_timeout(120)        # closeModal 后页面留 30ms 再泵下一条（app.js:76）
        if modal_visible(page):
            wait_element(page, Wait("element", "modal_body", expect="visible", timeout_ms=5000))
            log("   ↳ 队列下一条已显示（#modalBody visible）")
        else:
            wait_element(page, Wait("element", "modal_mask", expect="hidden", timeout_ms=5000))
            log("   ↳ 队列抽干，遮罩已隐藏（#modalMask hidden）")
    if modal_visible(page):
        raise PackError("队列关闭完记录里的 × 次数后弹窗仍未清空")


def phase_b(page: Any) -> None:
    """B：页签 → 查询 → 全选 → 打印 ×4（3 分支）→ 逐条关提示 → 手机号 → 确定。"""
    log("═══ 阶段 B：详情单补打 ═══")
    acts = acts_of("B")
    i = 0
    while i < len(acts):
        act = acts[i]
        if act.key == "B.×":
            j = i
            while j < len(acts) and acts[j].key == "B.×":
                j += 1
            drain_modal_queue(page, acts[i:j])
            i = j
            continue
        if act.key == "B.查询":
            gap("print_nos", "所以脚本没有输入待补打单号；真实平台这里必须先填 printNos 才能查询。")
        elif act.key == "B.on" and act.kind == "change":
            log(f"seq={act.seq} [B] change 'on' → 由前一次点击原生触发，只断言勾选态")
            if not page.locator(SEL["B.on"].primary).is_checked():
                raise PackError("全选复选框未勾选")
            i += 1
            continue
        elif act.key == "B.确定":
            gap("phone_input", "所以 /api/print-phone 会带空 phone；模拟站返回 200，真实平台大概率会校验。")
            if not modal_visible(page):
                log("   ! 手机号弹窗没出现：打印按钮的分支计数可能与录制时不同（NOTES §4）")
        perform(page, act)
        i += 1


def phase_c(page: Any) -> None:
    """C：页签 → 退回 ×2 → 妥投确认 → 关闭。"""
    log("═══ 阶段 C：退回寄件人 ═══")
    acts = acts_of("C")
    for act in acts:
        if act.key == "C.退回寄件人" and act.prefer == "#btnReturn":
            gap("return_mail_no",
                "录制的 C 阶段只有点击、没有输入动作；脚本不猜单号，也不自造定位器。")
        perform(page, act)
        if act.key == "C.妥投确认":
            assert_modal_contains(page, "妥投确认完成")
    log("提示：/api/delivered 的 count 取页面已退回条数（RAW/mirror/app.js:214），"
        "退回 2 条时才与 API.md 的样本 {\"count\":2} 一致。")


def phase_d(page: Any) -> None:
    """D：页签 → →（/api/handover）。"""
    log("═══ 阶段 D：投递归班交接 ═══")
    for act in acts_of("D"):
        perform(page, act)
    gap("panel_d", "D 阶段结束态（“已交接”）无法做文本断言，只能靠 /api/handover 的响应证明。")


PHASES: dict[str, Callable[[Any, Config], None]] = {
    "A": lambda page, cfg: phase_a(page, cfg.numbers),
    "B": lambda page, cfg: phase_b(page),
    "C": lambda page, cfg: phase_c(page),
    "D": lambda page, cfg: phase_d(page),
}


# ════════════════════════════════════════════════════════════════════════════
# 9. 包一致性自检
# ════════════════════════════════════════════════════════════════════════════
def verify_pack(pack_dir: Path = PACK_DIR) -> list[str]:
    """把定位器表与动作表逐条和包对账；返回错误列表（空 = 通过）。"""
    errors: list[str] = []
    sel_json, act_json = load_pack(pack_dir)

    jsel = {s["key"]: s for s in sel_json["selectors"]}
    if set(jsel) != set(SEL):
        errors.append(f"SELECTORS key 不一致：脚本多 {sorted(set(SEL) - set(jsel))} / "
                      f"包多 {sorted(set(jsel) - set(SEL))}")
    for key, mine in SEL.items():
        j = jsel.get(key)
        if not j:
            continue
        if j["primary"] != mine.primary:
            errors.append(f"[{key}] primary 不一致：脚本 {mine.primary!r} / 包 {j['primary']!r}")
        if tuple(j["backup"]) != mine.backup:
            errors.append(f"[{key}] backup 不一致：脚本 {mine.backup} / 包 {tuple(j['backup'])}")
        if bool(j["unique"]) != mine.unique:
            errors.append(f"[{key}] unique 不一致")
        if j["stability"] != mine.stability:
            errors.append(f"[{key}] stability 不一致")

    jact = {a["seq"]: a for a in act_json["actions"]}
    for act in ACTS:
        j = jact.get(act.seq)
        if not j:
            errors.append(f"seq={act.seq} 在包里不存在（build_actions 与包不同步）")
            continue
        allowed = {SEL[act.key].primary, *SEL[act.key].backup}
        if j["selector"]["primary"] not in allowed:
            errors.append(f"seq={act.seq} 记录的 primary={j['selector']['primary']!r} "
                          f"不在 key={act.key} 的 primary/backup 里")
        if act.prefer and act.prefer not in allowed:
            errors.append(f"seq={act.seq} prefer={act.prefer!r} 不在 key={act.key} 的 primary/backup 里")

    # 等待目标必须真的出现在包的 next_waits_for 里（证明不是脚本自创的 selector）
    recorded_wait_sels = {a.get("next_waits_for", {}).get("selector")
                          for a in act_json["actions"] if a.get("next_waits_for", {}).get("type") == "element"}
    for name, t in WAIT_TARGETS.items():
        if t.selector not in recorded_wait_sels:
            errors.append(f"等待目标 {name}={t.selector!r} 不在 ACTIONS.json 的 next_waits_for 里")

    errors += [f"脚本漏掉包里的动作：{x}" for x in LEFTOVERS]
    return errors


def direction_deviations(pack_dir: Path = PACK_DIR) -> list[tuple[int, str, str]]:
    """列出“元素等待方向 ≠ ui_delta 记录的可见性”的动作，供 --selfcheck 人工复核。"""
    try:
        _, act_json = load_pack(pack_dir)
    except PackError:
        return []
    jact = {a["seq"]: a for a in act_json["actions"]}
    out: list[tuple[int, str, str]] = []
    for act in ACTS:
        if act.wait.kind != "element":
            continue
        j = jact.get(act.seq) or {}
        rec_sel = (j.get("next_waits_for") or {}).get("selector")
        added = [x for x in ((j.get("ui_delta") or {}).get("added") or [])
                 if x.get("selector") == rec_sel]
        if not added:
            continue
        rec_dir = "hidden" if all("hidden" in (x.get("cls") or "") for x in added) else "visible"
        if rec_dir != act.wait.expect:
            out.append((act.seq, rec_dir, act.wait.expect))
    return out


def describe_wait(wait: Wait) -> str:
    if wait.kind == "request":
        return f"request {wait.method} {wait.path}（{wait.timeout_ms}ms）"
    if wait.kind == "element":
        return f"element {WAIT_TARGETS[wait.target].selector} {wait.expect}（{wait.timeout_ms}ms）"
    return f"none（静默 {CFG.none_settle_ms}ms）"


def print_plan(phases: list[str]) -> None:
    log(f"计划：base_url={CFG.base_url} phases={','.join(phases)} 单号={len(CFG.numbers)} 个")
    for act in ACTS:
        if act.step not in phases:
            continue
        pref = f" prefer={act.prefer!r}" if act.prefer else ""
        print(f"  {act.step} seq={act.seq:<3} {act.kind:<6} key={act.key:<16} "
              f"primary={SEL[act.key].primary!r}{pref}\n      ↳ {describe_wait(act.wait)}")


# ════════════════════════════════════════════════════════════════════════════
# 10. 浏览器装配 + CLI
# ════════════════════════════════════════════════════════════════════════════
def attach_dialog_policy(page: Any) -> None:
    """ACTIONS.json 没把 js 弹窗记成动作，但它们真实存在且会阻塞 Playwright：
         alert『打印任务已提交（模拟）』（RAW/dialogs.json seq=2，触发于 B 阶段第 1 次打印）
         confirm『确认将选中记录标记为妥投？』（seq=3，触发于 C 阶段妥投确认）
    confirm 若被 dismiss，/api/delivered 根本不会发出。统一 accept。"""

    def on_dialog(dialog: Any) -> None:
        log(f"   · js dialog [{dialog.type}] {dialog.message!r} → accept")
        try:
            dialog.accept()
        except Exception as exc:
            log(f"   ! dialog accept 失败：{exc}")

    page.on("dialog", on_dialog)


def read_numbers(path: str | None, inline: str) -> list[str]:
    """单号来源。包里只有脱敏占位值（7100***********），不能当真实单号用。"""
    if path:
        p = Path(path)
        if not p.exists():
            raise PackError(f"单号文件不存在：{p}")
        raw = p.read_text(encoding="utf-8")
    else:
        raw = inline
    return [x.strip() for x in re.split(r"[\s,;]+", raw) if x.strip()]


def run(phases: list[str]) -> int:
    from playwright.sync_api import sync_playwright   # 延迟导入：selfcheck/dry-run 不需要

    if not CFG.numbers:
        raise PackError("没有单号：用 --numbers-file 或 --numbers 指定（包里的 7100*********** 是脱敏占位）")

    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    artifact = Path(CFG.artifact_dir or Path(__file__).resolve().parent / "run-artifacts" / stamp)
    artifact.mkdir(parents=True, exist_ok=True)
    CFG.artifact_dir = artifact
    log(f"产物目录：{artifact}")

    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=not CFG.headed, slow_mo=CFG.slow_mo_ms)
        ctx = browser.new_context(viewport={"width": 1440, "height": 900})
        page = ctx.new_page()
        page.set_default_timeout(15000)
        attach_dialog_policy(page)
        try:
            log(f"打开 {CFG.base_url}")
            page.goto(CFG.base_url, wait_until="domcontentloaded")
            for step in phases:
                PHASES[step](page, CFG)
            log("全部阶段完成")
            return 0
        except Exception as exc:
            log(f"失败：{type(exc).__name__}: {exc}")
            try:
                page.screenshot(path=str(artifact / "failure.png"), full_page=True)
                (artifact / "failure.txt").write_text(str(exc), encoding="utf-8")
            except Exception:
                pass
            return 1
        finally:
            ctx.close()
            browser.close()


def parse_args(argv: list[str]) -> argparse.Namespace:
    ap = argparse.ArgumentParser(description="内网寄递业务平台·退货流程四阶段自动化骨架")
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL, help="平台地址（包里没有真实地址，必须传）")
    ap.add_argument("--numbers-file", default=None, help="单号清单文件（换行/逗号分隔）")
    ap.add_argument("--numbers", default="", help="直接给单号，逗号分隔")
    ap.add_argument("--phases", default="A,B,C,D", help="要跑的阶段，如 A 或 A,B,D")
    ap.add_argument("--headed", action="store_true", help="有头模式（实机调试）")
    ap.add_argument("--strict", action="store_true", help="触达 pack-gap 直接失败（实机推荐）")
    ap.add_argument("--screenshot-dir", default=None)
    ap.add_argument("--no-screenshot", action="store_true")
    ap.add_argument("--slow-mo", type=int, default=0)
    ap.add_argument("--none-settle-ms", type=int, default=400)
    ap.add_argument("--dry-run", action="store_true", help="只打印计划，不启动浏览器")
    ap.add_argument("--selfcheck", action="store_true", help="只校验脚本与包是否一致")
    return ap.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    CFG.base_url = args.base_url
    CFG.headed = args.headed
    CFG.strict = args.strict
    CFG.slow_mo_ms = args.slow_mo
    CFG.none_settle_ms = args.none_settle_ms
    CFG.screenshot = not args.no_screenshot
    CFG.artifact_dir = args.screenshot_dir
    CFG.numbers = read_numbers(args.numbers_file, args.numbers)

    phases = [p.strip().upper() for p in args.phases.split(",") if p.strip()]
    unknown = [p for p in phases if p not in PHASES]
    if unknown:
        print(f"未知阶段：{unknown}；可选 A/B/C/D", file=sys.stderr)
        return 2

    errors = verify_pack()
    if errors:
        log("包一致性校验失败：")
        for e in errors:
            log(f"  ✗ {e}")
        return 2
    log(f"包一致性校验通过：{len(SEL)} 个 SELECTORS key、{len(ACTS)} 个动作（seq 从 ACTIONS.json 解析）、"
        f"{len(WAIT_TARGETS)} 个等待目标")

    if args.selfcheck:
        for act in ACTS:
            print(f"  {act.step} seq={act.seq:<3} {act.kind:<6} key={act.key:<16} "
                  f"包记录={act.recorded:<26} 脚本={describe_wait(act.wait)}")
        devs = direction_deviations()
        print(f"\n元素等待方向与 ui_delta 相反的动作：{len(devs)} 条（理由见 NOTES §4）")
        for seq, rec, mine in devs:
            print(f"  seq={seq:<3} ui_delta={rec:<8} 脚本={mine}")
        return 0

    if args.dry_run:
        print_plan(phases)
        return 0

    return run(phases)


if __name__ == "__main__":
    raise SystemExit(main())
