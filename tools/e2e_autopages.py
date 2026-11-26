#!/usr/bin/env python3
""""自动巡页"自测：登录后工具自己打开 4 个页面。**非交付物。**"""
import json, pathlib, shutil, subprocess, sys, time, urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\debug\probe.exe")
OUT = PROBE / "out-e2e-autopages"
PORT = "9333"
NODE = shutil.which("node") or "node"


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
    mock = subprocess.Popen([sys.executable, "mock/serve.py", "8080"], cwd=str(PROBE),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    log = open(PROBE / "probe-autopages.log", "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen([str(EXE), "launch", "--out", str(OUT), "--port", PORT,
                          "--mirror-only", "--auto-pages"],
                         cwd=str(PROBE), stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
    checks = []

    def chk(name, cond, detail=""):
        checks.append(bool(cond))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + str(detail)) if detail else ''}")

    try:
        assert wait_port(), "端口没起来"
        time.sleep(1.5)
        # 模拟"操作员登录完成，落在业务首页"
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "nav",
                        "http://127.0.0.1:8080/"], capture_output=True, text=True, timeout=120)
        # 等自动巡页打开 4 页并抓完
        deadline = time.time() + 60
        sess = None
        while time.time() < deadline:
            time.sleep(2)
            dirs = sorted((OUT / "sessions").glob("*"))
            if dirs:
                sess = dirs[-1]
            txt = log.name and open(log.name, encoding="utf-8", errors="replace").read()
            if "抓取完成" in txt or "抓取可能仍在进行" in txt:
                break
        txt = open(log.name, encoding="utf-8", errors="replace").read()
        opened = txt.count("[自动巡页] 已打开：")
        chk("自动打开了 ≥3 个业务页面", opened >= 3, f"{opened} 个")
        chk("打印了『4 个业务页面已全部打开』", "4 个业务页面已全部打开" in txt)
        chk("打印了『可以关掉浏览器』", "可以直接关掉浏览器" in txt or "抓取完成" in txt,
            txt.strip().splitlines()[-1][:60] if txt.strip() else "")

        # 关浏览器收尾
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "close"],
                       capture_output=True, text=True, timeout=60)
        dl = time.time() + 40
        while time.time() < dl and p.poll() is None:
            time.sleep(0.5)
        chk("关浏览器后自动收尾", p.poll() == 0, p.poll())
        if sess:
            idx = sess / "mirror" / "INDEX.json"
            data = json.loads(idx.read_text(encoding="utf-8")) if idx.exists() else {}
            pages = [r.get("url", "") for r in data.get("resources", []) if "page=" in r.get("url", "")]
            chk("镜像里能查到按页面打开的 URL", len(pages) >= 3, f"{len(pages)} 个")
            chk("报告已生成", (sess / "report.md").exists())
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
