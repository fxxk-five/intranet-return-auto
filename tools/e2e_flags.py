#!/usr/bin/env python3
"""开关自测：`--no-filmstrip` / `--no-debugger` 下其余功能不受影响（PROMPT §9.2/§9.3）。**非交付物。**"""
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE_DIR = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\release\probe.exe")
OUT = PROBE_DIR / "out-e2e-flags"
PORT = "9333"
NODE = shutil.which("node") or r"C:\Users\电商\AppData\Local\Programs\nodejs-portable\node-v24.16.0-win-x64\node.exe"


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


def main():
    subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
    shutil.rmtree(OUT, ignore_errors=True)
    mock = subprocess.Popen([sys.executable, "mock/serve.py", "8080"], cwd=str(PROBE_DIR),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    log = open(PROBE_DIR / "probe-flags.log", "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen(
        [str(EXE), "launch", "--out", str(OUT), "--port", PORT, "--no-filmstrip", "--no-debugger"],
        cwd=str(PROBE_DIR), stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True,
    )
    checks = []

    def chk(name, cond, detail=""):
        checks.append(bool(cond))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + str(detail)) if detail else ''}")

    try:
        assert wait_port(), "端口没起来"
        time.sleep(1.5)
        for phase in ("nav", "A"):
            subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, phase,
                            "http://127.0.0.1:8080/"], capture_output=True, text=True, timeout=180)
        p.stdin.write("END\n")
        p.stdin.flush()
        deadline = time.time() + 30
        while time.time() < deadline and p.poll() is None:
            time.sleep(0.5)
        rc = p.poll()
        if rc is None:
            subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "close"], capture_output=True)
            time.sleep(3)
            rc = p.poll()
        chk("正常收尾", rc == 0, rc)

        sess = sorted((OUT / "sessions").glob("*"))[-1]
        film_dir = sess / "filmstrip"
        pngs = list(film_dir.glob("*.png")) if film_dir.exists() else []
        chk("没有 filmstrip 截图（--no-filmstrip）", len(pngs) == 0, f"{len(pngs)} 张")
        chk("报告仍然生成", (sess / "report.md").exists())
        idx = sess / "mirror" / "INDEX.json"
        chk("镜像仍然工作（resources 非空）", idx.exists() and '"resources": [' in idx.read_text(encoding="utf-8"))
        inl = sess / "mirror" / "inline_scripts"
        chk("--no-debugger 时仍能从 DOM 兜底抓内联脚本", inl.exists() and any(inl.glob("*.js")),
            f"{len(list(inl.glob('*.js'))) if inl.exists() else 0} 个")
    finally:
        try:
            p.kill()
        except Exception:
            pass
        mock.terminate()
        subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
        log.close()

    failed = checks.count(False)
    print(f"\n结果：{len(checks)-failed}/{len(checks)} 通过")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
