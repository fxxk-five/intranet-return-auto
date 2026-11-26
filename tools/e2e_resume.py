#!/usr/bin/env python3
"""断点续跑自测（§7.9 / §11.2 最后两项）。**不是交付物。**

流程：跑 A → 硬杀探针（模拟崩溃/Ctrl+C）→ 用同一个 --out 重启 → 跑 B → END 收尾
校验：session 目录仍然只有 1 个、latest 指向它、seq 不重复、报告能生成、有 finished 标记
"""

import json
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE_DIR = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\debug\probe.exe")
OUT = PROBE_DIR / "out-e2e-resume"
PORT = "9333"
NODE = shutil.which("node") or r"C:\Users\电商\AppData\Local\Programs\nodejs-portable\node-v24.16.0-win-x64\node.exe"
MOCK_URL = "http://127.0.0.1:8080/"


def wait_port(timeout=50):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=2) as r:
                r.read()
            return True
        except Exception:
            time.sleep(0.3)
    return False


def drive(phase):
    r = subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, phase, MOCK_URL],
                       capture_output=True, text=True, timeout=180)
    print("   drive", phase, "rc=", r.returncode, (r.stdout or r.stderr).strip()[:80])


def start_probe():
    log = open(PROBE_DIR / "probe-resume.log", "a", encoding="utf-8", errors="replace")
    p = subprocess.Popen([str(EXE), "launch", "--out", str(OUT), "--port", PORT],
                         cwd=str(PROBE_DIR), stdin=subprocess.PIPE, stdout=log,
                         stderr=subprocess.STDOUT, text=True)
    return p


def main():
    subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
    if OUT.exists():
        shutil.rmtree(OUT, ignore_errors=True)
    mock = subprocess.Popen([sys.executable, "mock/serve.py", "8080"], cwd=str(PROBE_DIR),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    checks = []

    def chk(name, cond, detail=""):
        checks.append(bool(cond))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + str(detail)) if detail else ''}")

    try:
        # ---- 第一段 ----
        p1 = start_probe()
        assert wait_port(), "端口没起来"
        time.sleep(1.5)
        drive("nav")
        drive("A")
        time.sleep(1)
        p1.kill()  # 硬杀 = 崩溃/断电，比 Ctrl+C 更狠
        p1.wait(timeout=10)
        print("  已硬杀第一段探针")
        subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
        time.sleep(2)

        sessions1 = sorted((OUT / "sessions").glob("*"))
        chk("第一段产生了 1 个 session", len(sessions1) == 1, [d.name for d in sessions1])
        latest1 = (OUT / "latest").read_text(encoding="utf-8").strip()
        lines1 = sum(1 for _ in open(sessions1[0] / "timeline.jsonl", encoding="utf-8"))

        # ---- 第二段（续跑） ----
        p2 = start_probe()
        assert wait_port(), "端口没起来（第二段）"
        time.sleep(1.5)
        drive("nav")
        drive("B")
        time.sleep(1)
        p2.stdin.write("END\n")
        p2.stdin.flush()
        deadline = time.time() + 30
        while time.time() < deadline and p2.poll() is None:
            time.sleep(0.5)
        rc = p2.poll()
        if rc is None:
            drive("close")
            deadline = time.time() + 20
            while time.time() < deadline and p2.poll() is None:
                time.sleep(0.5)
        rc = p2.poll()
        print("  第二段退出码", rc)
        chk("第二段正常收尾", rc == 0, rc)

        sessions2 = sorted((OUT / "sessions").glob("*"))
        chk("重启后没有新建 session（续跑）", len(sessions2) == 1, [d.name for d in sessions2])
        latest2 = (OUT / "latest").read_text(encoding="utf-8").strip()
        chk("latest 指向同一个 session", latest1 == latest2, f"{latest1} vs {latest2}")

        s = sessions2[0]
        rows = [json.loads(l) for l in open(s / "timeline.jsonl", encoding="utf-8")]
        seqs = [r["seq"] for r in rows]
        chk("续跑后事件数增加", len(rows) > lines1, f"{lines1} → {len(rows)}")
        chk("seq 无重复", len(seqs) == len(set(seqs)), f"{len(seqs) - len(set(seqs))} 个重复")
        chk("seq 单调递增", seqs == sorted(seqs))

        chk("有 finished 标记", (s / "finished").exists())
        chk("报告已生成", (s / "report.md").exists())
        md = (s / "report.md").read_text(encoding="utf-8") if (s / "report.md").exists() else ""
        chk("报告仍含十二节", md.count("## ") >= 12, md.count("## "))
    finally:
        try:
            p1.kill()
        except Exception:
            pass
        try:
            p2.kill()
        except Exception:
            pass
        mock.terminate()
        subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)

    failed = checks.count(False)
    print(f"\n结果：{len(checks) - failed}/{len(checks)} 通过")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
