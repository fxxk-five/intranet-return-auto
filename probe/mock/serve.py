#!/usr/bin/env python3
"""模拟寄递平台的本地开发服务器（**仅开发期使用，不进交付物**）。

用途：在没有内网的环境里，让 probe 能端到端跑完 §11.2 的自测清单。

    python mock/serve.py            # 监听 127.0.0.1:8080
    python mock/serve.py 8081       # 换端口

静态文件来自本目录；/api/* 一律返回 {"code":0,"msg":"成功",...}，
并把每次请求打到 stdout，方便核对"1 次点击 = 1 个请求 = 1 个单号"。
"""

import json
import os
import sys
import time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.abspath(__file__))
API_DELAY = 0.12  # 秒，模拟内网接口耗时


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def log_message(self, fmt, *args):
        sys.stdout.write("[%s] %s\n" % (time.strftime("%H:%M:%S"), fmt % args))

    def do_GET(self):
        if self.path == "/":
            self.path = "/index.html"
        if self.path == "/api/version":
            body = json.dumps({"version": "mock-1.0", "ts": int(time.time() * 1000)}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        return super().do_GET()

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            payload = json.loads(raw.decode("utf-8")) if raw else {}
        except Exception:
            payload = {"_raw": raw.decode("utf-8", "replace")}

        # 单号计数：逗号分隔的算多个（供 H3/协议重放观察）
        count = 1
        for key in ("mailNo", "mailNos"):
            v = payload.get(key)
            if isinstance(v, str):
                count = max(count, len([x for x in v.split(",") if x.strip()]))
            elif isinstance(v, list):
                count = max(count, len(v))

        print("[%s] POST %s count=%d payload=%s"
              % (time.strftime("%H:%M:%S"), self.path, count, json.dumps(payload, ensure_ascii=False)))
        sys.stdout.flush()

        time.sleep(API_DELAY)
        body = json.dumps(
            {"code": 0, "msg": "成功", "count": count, "serverTime": int(time.time() * 1000)},
            ensure_ascii=False,
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print("模拟页面：http://127.0.0.1:%d/" % port)
    print("H1 模式：http://127.0.0.1:%d/?mode=h1" % port)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n已停止")


if __name__ == "__main__":
    main()
