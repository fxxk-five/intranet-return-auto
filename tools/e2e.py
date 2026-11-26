#!/usr/bin/env python3
"""端到端自测（§11.2）：mock 页 + probe launch + CDP 驱动 + 产物校验。

**不是交付物**，只在开发机上用。

    python tools/e2e.py default      # 期望 q2_verdict == H2
    python tools/e2e.py h1           # 期望 q2_verdict == H1（?mode=h1）

一次只跑一遍；脚本自己负责起停 mock 服务 / Edge / probe。
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE_DIR = ROOT / "probe"
EXE = pathlib.Path(os.environ.get("PROBE_EXE", r"D:\probe-target\debug\probe.exe"))
PORT = "9333"
MOCK_PORT = "8080"
MODE = sys.argv[1] if len(sys.argv) > 1 else "default"
OUT = PROBE_DIR / ("out-e2e-" + MODE)
MOCK_URL = f"http://127.0.0.1:{MOCK_PORT}/" + ("?mode=h1" if MODE == "h1" else "")

NODE = shutil.which("node") or r"C:\Users\电商\AppData\Local\Programs\nodejs-portable\node-v24.16.0-win-x64\node.exe"


def http_json(path):
    with urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=3) as r:
        return json.loads(r.read().decode("utf-8"))


def wait_port(timeout=60):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            http_json("/json/version")
            return True
        except Exception:
            time.sleep(0.3)
    return False


def sendkeys(combo):
    ps = (
        "Add-Type -AssemblyName System.Windows.Forms; "
        f"[System.Windows.Forms.SendKeys]::SendWait('{combo}')"
    )
    subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True)


def drive(phase):
    r = subprocess.run(
        [NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, phase, MOCK_URL],
        capture_output=True,
        text=True,
        timeout=180,
    )
    print(f"    drive {phase}: rc={r.returncode} {(r.stdout or r.stderr).strip()[:300]}")
    return r.returncode == 0


def kill_edges():
    subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)


def main():
    print(f"=== e2e mode={MODE} out={OUT} ===")
    kill_edges()
    if OUT.exists():
        shutil.rmtree(OUT, ignore_errors=True)

    mock_log = open(PROBE_DIR / f"mock-{MODE}.log", "w", encoding="utf-8", errors="replace")
    mock = subprocess.Popen(
        [sys.executable, "mock/serve.py", MOCK_PORT],
        cwd=str(PROBE_DIR),
        stdout=mock_log,
        stderr=subprocess.STDOUT,
    )
    time.sleep(1.0)

    probe_log = open(PROBE_DIR / f"probe-{MODE}.log", "w", encoding="utf-8", errors="replace")
    probe = subprocess.Popen(
        [
            str(EXE), "launch", "--out", str(OUT), "--port", PORT,
            "--numbers", str(PROBE_DIR / "mock" / "numbers.txt"),
            "--values", str(PROBE_DIR / "mock" / "values.json"),
            "--annotate", "--guide",
            "--plan", str(PROBE_DIR / "plan.json"),
        ],
        cwd=str(PROBE_DIR),
        stdin=subprocess.PIPE,
        stdout=probe_log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    ok = False
    try:
        if not wait_port(50):
            print("  [FAIL] 调试端口没起来，看 probe-*.log")
            return 1
        time.sleep(1.5)

        if not drive("nav"):
            return 1
        # 先切到 A 阶段，再标一个 SOP 动作（#2 点确定），验证标注链路
        probe.stdin.write("A\n")
        probe.stdin.flush()
        time.sleep(0.4)
        probe.stdin.write("2\n")
        probe.stdin.flush()
        time.sleep(0.4)
        drive("A")

        for step_cmd, phase in [("B", "B"), ("C", "C"), ("D", "D")]:
            probe.stdin.write(step_cmd + "\n")
            probe.stdin.flush()
            time.sleep(1.0)
            drive(phase)

        # 收尾：优先走 stdin（热键在这些环境下不可靠），再关浏览器兜底
        probe.stdin.write("END\n")
        probe.stdin.flush()
        deadline = time.time() + 25
        while time.time() < deadline and probe.poll() is None:
            time.sleep(0.5)

        if probe.poll() is None:
            print("  [WARN] stdin 收尾没生效，改用关闭浏览器")
            drive("close")
            deadline = time.time() + 25
            while time.time() < deadline and probe.poll() is None:
                time.sleep(0.5)

        if probe.poll() is None:
            print("  [FAIL] probe 没有自己结束，强制终止（产物不完整）")
            probe.terminate()
            time.sleep(2)
        else:
            ok = True
            print(f"  probe 退出码 {probe.returncode}")
    finally:
        try:
            probe.kill()
        except Exception:
            pass
        mock.terminate()
        kill_edges()
        probe_log.close()
        mock_log.close()

    return verify(ok)


def load(p):
    try:
        return json.loads(pathlib.Path(p).read_text(encoding="utf-8"))
    except Exception:
        return None


def verify(probe_ok):
    print("\n=== §11.2 自测清单 ===")
    sess_dirs = sorted((OUT / "sessions").glob("*")) if (OUT / "sessions").exists() else []
    if not sess_dirs:
        print("  [FAIL] 没有 session 目录")
        return 1
    s = sess_dirs[-1]
    checks = []

    def chk(name, cond, detail=""):
        checks.append((name, bool(cond), detail))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + detail) if detail else ''}")

    chk("probe 正常收尾", probe_ok)
    chk("timeline.jsonl 有事件", (s / "timeline.jsonl").exists() and
        sum(1 for _ in open(s / "timeline.jsonl", encoding="utf-8")) > 20,
        f"{sum(1 for _ in open(s / 'timeline.jsonl', encoding='utf-8')) if (s/'timeline.jsonl').exists() else 0} 行")

    elements = load(s / "elements.json") or {}
    cm = elements.get("control_map", {})
    mail = cm.get("邮件号码输入框") or cm.get("邮件号码") or {}
    sel = None
    for fr in elements.get("frames", []):
        for el in fr.get("elements", []):
            if el.get("attrs", {}).get("name") == "mailNo":
                sel = el
    uniq = sel and all(x.get("unique") for x in sel.get("selectors", [])[:3])
    chk("elements.json 里 input[name=mailNo] 的 3 个 selector 全部唯一", uniq,
        f"{len(sel.get('selectors', [])) if sel else 0} 个候选")

    har = load(s / "network.har") or {}
    entries = har.get("log", {}).get("entries", [])
    scan = [e for e in entries if "/api/scan" in e.get("request", {}).get("url", "")]
    chk("network.har 有 POST /api/scan", bool(scan), f"{len(scan)} 条")

    corr = [e for e in entries if e.get("_probe", {}).get("causedByClickSeq")]
    chk("click 与 request 通过 caused_by_click_seq 关联", len(corr) >= 1, f"{len(corr)} 条已关联")

    exp = load(s / "experiment21.json") or {}
    q2 = exp.get("q2_verdict")
    want = "H1" if MODE == "h1" else "H2"
    chk(f"q2_verdict == {want}", q2 == want, f"实际 {q2}")

    dialogs = load(s / "dialogs.json") or {}
    cnt = dialogs.get("classifications", {})
    chk("alert 归为 js_dialog", cnt.get("js_dialog", 0) >= 1, f"js_dialog={cnt.get('js_dialog', 0)}")
    chk("DOM 模态框归为 dom_modal", cnt.get("dom_modal", 0) >= 1, f"dom_modal={cnt.get('dom_modal', 0)}")
    shots = list((s / "screenshots").glob("*.png")) if (s / "screenshots").exists() else []
    chk("有截图（含弹窗）", len(shots) >= 2, f"{len(shots)} 张")

    md = (s / "report.md").read_text(encoding="utf-8") if (s / "report.md").exists() else ""
    need = ["## 一、一句话结论", "## 二、SOP 一致性", "## 三、Q1 请求取证", "## 四、Q2 关于",
            "## 五、Q3 控件定位器", "## 六、Q4 页面结构", "## 七、Q5 打印分支判定",
            "## 八、Q6 性能", "## 九、Q7 打印环境", "## 十、操作序列时间线",
            "## 十一、源码镜像", "## 十二、弹窗清单", "## 十三、风险清单",
            "## 十四、后续三个月路线建议", "## 十五、附录"]
    missing = [n for n in need if n not in md]
    chk("report.md 十二节齐全", not missing, f"缺 {missing}" if missing else "")

    # privacy
    import re
    leaked = re.findall(r"\b710000000004\d{3}\b", md)
    chk("报告里没有未脱敏单号", not leaked, f"泄漏 {leaked[:3]}" if leaked else "")

    # assertions
    chk("assertions.json 生成", (s / "assertions.json").exists())

    # ---- 影印机：源码镜像 ----
    mirror = s / "mirror"
    idx = load(mirror / "INDEX.json") or {}
    res = idx.get("resources", [])
    urls = [r.get("url", "") for r in res]
    chk("mirror 抓到 app.js", any(u.endswith("app.js") for u in urls), f"{len(urls)} 个资源")
    chk("mirror 抓到 style.css", any(u.endswith("style.css") for u in urls))
    summ = idx.get("summary", {})
    chk(
        "INDEX.json total == captured + skipped",
        summ.get("total", -1) == summ.get("captured", 0) + summ.get("skipped", 0),
        f"{summ}",
    )
    app_js = list(mirror.rglob("app.js"))
    chk("app.js 落盘且非空", bool(app_js) and app_js[0].stat().st_size > 1000,
        f"{app_js[0].stat().st_size} B" if app_js else "")
    mhtmls = list((mirror / "mhtml").glob("*.mhtml")) if (mirror / "mhtml").exists() else []
    chk("有 MHTML 存档", len(mhtmls) >= 1, f"{len(mhtmls)} 份")
    inlines = list((mirror / "inline_scripts").glob("*.js")) if (mirror / "inline_scripts").exists() else []
    chk("抓到运行时脚本（eval 出来的）", len(inlines) >= 1, f"{len(inlines)} 个")

    # ---- 快照带 ----
    film = s / "filmstrip"
    pngs = list(film.glob("*.png")) if film.exists() else []
    htmls = list(film.glob("*.html")) if film.exists() else []
    chk("filmstrip 截图 ≥ 40 张", len(pngs) >= 40, f"{len(pngs)} 张")
    chk("filmstrip 完整 DOM ≥ 20 份", len(htmls) >= 20, f"{len(htmls)} 份")
    big = sorted(htmls)[-1].read_text(encoding="utf-8", errors="replace") if htmls else ""
    chk("完整 DOM 是合法 HTML（未被字节截断）",
        big.strip().endswith(">") or big.strip().endswith("<!-- TRUNCATED -->"),
        big.strip()[-40:].replace("\n", " "))

    # ---- 数据入口 ----
    inp = load(s / "inputs.json") or {}
    used = [n for n in inp.get("numbers", []) if n.get("used")]
    chk("inputs.json 记录了已用单号", len(used) >= 2, f"{len(used)} 个已用")
    tl = (s / "timeline.jsonl").read_text(encoding="utf-8")
    chk("有 input_number 事件", '"kind":"input_number"' in tl or '"kind": "input_number"' in tl)

    # ---- 标注 ----
    chk("有 annotation 事件", '"kind":"annotation"' in tl or '"kind": "annotation"' in tl)

    # ---- 报告新增两节 ----
    chk("报告有『操作序列时间线』", "## 十、操作序列时间线" in md)
    chk("报告有『SOP 一致性』", "## 二、SOP 一致性" in md)
    chk("报告有『源码镜像』", "## 十一、源码镜像" in md)

    failed = [c for c in checks if not c[1]]
    print(f"\n结果：{len(checks) - len(failed)}/{len(checks)} 通过")
    print(f"产物目录：{s}")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
