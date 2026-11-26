#!/usr/bin/env python3
"""定位 pointerdown 链路问题的最小复现（非交付物）。"""
import pathlib, shutil, subprocess, sys, time, urllib.request, json

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "probe"
EXE = pathlib.Path(r"D:\probe-target\debug\probe.exe")
OUT = PROBE / "out-ptr2"
PORT = "9333"
NODE = shutil.which("node")


def wait_port(t=40):
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
    log = open(PROBE / "ptr2.log", "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen([str(EXE), "launch", "--out", str(OUT), "--port", PORT],
                         cwd=str(PROBE), stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
    try:
        assert wait_port(), "no port"
        time.sleep(1.5)
        script = r'''
const PORT=process.argv[1];
(async()=>{
 const list=await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
 const page=list.find(t=>t.type==='page');
 const ws=new WebSocket(page.webSocketDebuggerUrl);
 await new Promise(r=>ws.onopen=r);
 let id=0; const pend=new Map();
 ws.onmessage=e=>{const m=JSON.parse(e.data); if(m.id&&pend.has(m.id)){pend.get(m.id)(m);pend.delete(m.id);}};
 const send=(m,pa={})=>new Promise(res=>{const i=++id;pend.set(i,res);ws.send(JSON.stringify({id:i,method:m,params:pa}));});
 const ev=async(x)=>{const r=await send('Runtime.evaluate',{expression:x,returnByValue:true,awaitPromise:true});return r.result&&r.result.result?r.result.result.value:undefined;};
 await send('Page.enable'); await send('Runtime.enable');
 await send('Page.navigate',{url:'http://127.0.0.1:8080/'});
 await new Promise(r=>setTimeout(r,2000));
 console.log('probe fn:', await ev('typeof window.__probe'));
 console.log('installed flag:', await ev('window.__probeInstalled'));
 // 1) 手动派发 pointerdown（应被记录）
 console.log('manual dispatch:', await ev('(function(){var b=document.getElementById("btnOk"); b.dispatchEvent(new PointerEvent("pointerdown",{bubbles:true,cancelable:true})); return "sent";})()'));
 await new Promise(r=>setTimeout(r,600));
 // 2) 装 prototype 覆盖后再 click（应被记录）
 console.log('override:', await ev('(function(){ if(window.__pc2){return "already"} window.__pc2=1; var o=Element.prototype.click; Element.prototype.click=function(){ try{this.dispatchEvent(new PointerEvent("pointerdown",{bubbles:true,cancelable:true}));}catch(e){} return o.apply(this,arguments)}; return "ok";})()'));
 await new Promise(r=>setTimeout(r,300));
 console.log('click:', await ev('(function(){document.getElementById("btnOk").click(); return "clicked"})()'));
 await new Promise(r=>setTimeout(r,800));
 ws.close();
})();'''
        r = subprocess.run([NODE, "-e", script, PORT], capture_output=True, text=True, timeout=90)
        print(r.stdout.strip())
        if r.stderr.strip():
            print("STDERR", r.stderr.strip()[:400])
        time.sleep(1)
        sess = sorted((OUT / "sessions").glob("*"))[-1]
        tl = (sess / "timeline.jsonl").read_text(encoding="utf-8")
        print("pointer_down:", tl.count('"kind":"pointer_down"'))
        print("click:", tl.count('"kind":"click"'))
        print("methods:", [l.split('"before_method":"')[1].split('"')[0] for l in tl.splitlines() if 'before_method' in l][:5])
    finally:
        p.stdin.write("END\n"); p.stdin.flush()
        time.sleep(3)
        try: p.kill()
        except Exception: pass
        mock.terminate()
        subprocess.run(["taskkill", "/F", "/IM", "msedge.exe"], capture_output=True)
        log.close()


if __name__ == "__main__":
    main()
