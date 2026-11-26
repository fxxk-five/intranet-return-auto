#!/usr/bin/env python3
"""场景自测：页面**先打开**、探针后连上（attach），源码还能不能抓全。**非交付物。**"""
import json, pathlib, shutil, subprocess, sys, time, urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\debug\probe.exe")
OUT = PROBE / "out-e2e-preopen"
PROFILE = pathlib.Path(r"D:\tmp-profile-preopen")
PORT = "9333"
NODE = shutil.which("node") or "node"
EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"


def wait_port(t=50):
    t0 = time.time()
    while time.time() - t0 < t:
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=2).read()
            return True
        except Exception:
            time.sleep(0.3)
    return False


def main():
    subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
    shutil.rmtree(OUT, ignore_errors=True)
    shutil.rmtree(PROFILE, ignore_errors=True)
    PROFILE.mkdir(parents=True, exist_ok=True)
    mock = subprocess.Popen([sys.executable, "mock/serve.py", "8080"], cwd=str(PROBE),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)

    # 1) 手工起 Edge（模拟"操作员已经开好了浏览器"）
    edge = subprocess.Popen([
        EDGE, f"--remote-debugging-port={PORT}", "--remote-allow-origins=*",
        f"--user-data-dir={PROFILE}", "--no-first-run", "--no-default-browser-check",
        "about:blank",
    ])
    checks = []
    log = None
    probe = None

    def chk(name, cond, detail=""):
        checks.append(bool(cond))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + str(detail)) if detail else ''}")

    try:
        assert wait_port(), "Edge 端口没起来"
        time.sleep(1.5)
        # 2) 页面已经打开并加载完（探针还没连）
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "nav",
                        "http://127.0.0.1:8080/"], capture_output=True, text=True, timeout=120)
        time.sleep(2)

        # 3) 现在才连探针（attach，不是 launch）
        log = open(PROBE / "probe-preopen.log", "w", encoding="utf-8", errors="replace")
        probe = subprocess.Popen(
            [str(EXE), "attach", "--out", str(OUT), "--port", PORT, "--mirror-only"],
            cwd=str(PROBE), stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
        time.sleep(6)  # 给 tick 循环两三个回合（此时产物还没写盘，INDEX 要到收尾才生成）

        # 4) 操作员关浏览器 = 收尾
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "close"],
                       capture_output=True, text=True, timeout=60)
        deadline = time.time() + 40
        while time.time() < deadline and probe.poll() is None:
            time.sleep(0.5)
        chk("关浏览器后自动收尾", probe.poll() == 0, probe.poll())

        sess = sorted((OUT / "sessions").glob("*"))[-1]
        chk("报告已生成", (sess / "report.md").exists())
        idx_path = sess / "mirror" / "INDEX.json"
        idx = json.loads(idx_path.read_text(encoding="utf-8")) if idx_path.exists() else {}
        urls = [r.get("url", "") for r in idx.get("resources", [])]
        chk("后连上也能抓到 app.js", any(u.endswith("app.js") for u in urls), f"{len(urls)} 个资源")
        chk("后连上也能抓到 style.css", any(u.endswith("style.css") for u in urls))
        chk("抓到页面 HTML", bool(list((sess / "mirror").rglob("*.html"))))
        els = json.loads((sess / "elements.json").read_text(encoding="utf-8")) if (sess / "elements.json").exists() else {}
        n = sum(len(f.get("elements", [])) for f in els.get("frames", []))
        chk("元素清单已预采集", n > 0, f"{n} 个元素")
    finally:
        for p in (probe,):
            try:
                if p: p.kill()
            except Exception:
                pass
        try:
            edge.kill()
        except Exception:
            pass
        mock.terminate()
        subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
        if log: log.close()

    failed = checks.count(False)
    print(f"\n结果：{len(checks)-failed}/{len(checks)} 通过")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
