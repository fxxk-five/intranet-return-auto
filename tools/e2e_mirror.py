#!/usr/bin/env python3
""""只扒源码"模式自测（--mirror-only）：操作员零按键也能出可用产物。**非交付物。**"""
import json, pathlib, shutil, subprocess, sys, time, urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\debug\probe.exe")
OUT = PROBE / "out-e2e-mirror"
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
    log = open(PROBE / "probe-mirror.log", "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen([str(EXE), "launch", "--out", str(OUT), "--port", PORT, "--mirror-only"],
                         cwd=str(PROBE), stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
    checks = []

    def chk(name, cond, detail=""):
        checks.append(bool(cond))
        print(f"  {'[OK]  ' if cond else '[FAIL]'} {name}{(' — ' + str(detail)) if detail else ''}")

    try:
        assert wait_port(), "端口没起来"
        time.sleep(1.5)
        # 操作员动作：打开页面（这里只打开一页，真实场景是 4 页）
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "nav",
                        "http://127.0.0.1:8080/"], capture_output=True, text=True, timeout=120)
        time.sleep(3)  # 等页面加载完 + 探针抓取
        # "关浏览器"= 结束
        subprocess.run([NODE, str(ROOT / "tools" / "drive_mock.mjs"), PORT, "close"],
                       capture_output=True, text=True, timeout=60)
        deadline = time.time() + 40
        while time.time() < deadline and p.poll() is None:
            time.sleep(0.5)
        rc = p.poll()
        chk("关浏览器后自动收尾", rc == 0, rc)

        sess = sorted((OUT / "sessions").glob("*"))[-1]
        mirror = sess / "mirror"
        idx = json.loads((mirror / "INDEX.json").read_text(encoding="utf-8")) if (mirror / "INDEX.json").exists() else {}
        urls = [r.get("url", "") for r in idx.get("resources", [])]
        chk("镜像抓到 app.js", any(u.endswith("app.js") for u in urls), f"{len(urls)} 个资源")
        chk("镜像抓到 style.css", any(u.endswith("style.css") for u in urls))
        chk("镜像抓到页面 HTML", any(u.endswith("/") or u.endswith("index.html") or r.get("kind") == "Document"
                                   for u, r in zip(urls, idx.get("resources", []))) or bool(list(mirror.rglob("*.html"))))
        chk("有 MHTML", bool(list((mirror / "mhtml").glob("*.mhtml"))) if (mirror / "mhtml").exists() else False)
        chk("没有 filmstrip（零按键模式不录像）", not (sess / "filmstrip").exists()
            or not any((sess / "filmstrip").glob("*.png")))
        els = json.loads((sess / "elements.json").read_text(encoding="utf-8")) if (sess / "elements.json").exists() else {}
        n = sum(len(f.get("elements", [])) for f in els.get("frames", []))
        chk("关浏览器后仍有元素清单（预采集生效）", n > 0, f"{n} 个元素")
        chk("报告已生成", (sess / "report.md").exists())
        pages_dir = sess / "pages"
        docs = list(pages_dir.rglob("*.html")) if pages_dir.exists() else []
        chk("落了完整页面 DOM（含 iframe）", len(docs) >= 2, f"{len(docs)} 份")
        has_frame = any("投递下段主界面" in d.read_text(encoding="utf-8", errors="replace") for d in docs)
        chk("iframe 里的业务页也被抓到", has_frame)
        tl = (sess / "timeline.jsonl").read_text(encoding="utf-8")
        chk("Document 正文已落盘并记路径", '"body_file":"pages/' in tl)
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
