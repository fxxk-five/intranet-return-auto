// 自测用的浏览器驱动（**不是交付物**）：用 CDP 操作 mock 页面，模拟操作员点击。
// 用法：node tools/drive_mock.mjs <port> <phase> [url]
//   phase: nav | A | B | C | D | close
//
// 注意：这是测试脚手架。探针本身绝不自动点击业务按钮（§15.2）。
//
// 关键点：探针的"前"快照挂在 pointerdown 上，程序化 click 不会触发它，
// 所以每次点击都先显式派发一次 pointerdown（真实鼠标天然就有）。

const PORT = process.argv[2] || '9333';
const PHASE = process.argv[3] || 'nav';
const MOCK = process.argv[4] || 'http://127.0.0.1:8080/';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
  const page = list.find((t) => t.type === 'page' && !t.url.startsWith('devtools://'));
  if (!page) throw new Error('没有找到 page target');

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = () => rej(new Error('ws error'));
  });

  let id = 0;
  const pending = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) {
      pending.get(m.id)(m);
      pending.delete(m.id);
    }
  };
  const send = (method, params = {}) =>
    new Promise((res, rej) => {
      const i = ++id;
      pending.set(i, res);
      ws.send(JSON.stringify({ id: i, method, params }));
      setTimeout(() => rej(new Error('timeout ' + method)), 30000);
    });
  const evalJs = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.error) throw new Error(JSON.stringify(r.error));
    return r.result && r.result.result ? r.result.result.value : undefined;
  };

  // 点一个选择器：派发 pointerdown（探针取"前"快照）再 click
  const tap = async (selector) => {
    const ok = await evalJs(
      `(function(){var e=document.querySelector(${JSON.stringify(selector)});
        if(!e) return false;
        try { e.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,cancelable:true})); } catch(x) {}
        e.click(); return true;})()`
    );
    if (!ok) throw new Error('tap 找不到元素：' + selector);
    return true;
  };

  const setVal = async (selector, value, events = 'input,change') => {
    await evalJs(
      `(function(){var e=document.querySelector(${JSON.stringify(selector)});
        if(!e) return false;
        e.value=${JSON.stringify(value)};
        ${events.split(',').map((ev) => `e.dispatchEvent(new Event('${ev}',{bubbles:true}));`).join('')}
        return true;})()`
    );
  };

  await send('Page.enable');
  await send('Runtime.enable');

  if (PHASE === 'nav') {
    await send('Page.navigate', { url: MOCK });
    await sleep(1800);
    console.log('navigated', await evalJs('location.href'));
  }

  if (PHASE === 'A') {
    for (let i = 1; i <= 3; i++) {
      await setVal('#mailNo', `71000000000${4140 + i}`);
      await tap('#btnOk');          // 每次"确定"= 1 请求 = 1 单号（验证 H2）
      await sleep(700);
      await tap('#modalClose');     // 关掉"下段扫描成功"提示
      await sleep(400);
    }
    await tap('#confirmScan');
    await sleep(800);
    await tap('#modalClose');
    await sleep(300);
    console.log('phase A done');
  }

  if (PHASE === 'B') {
    await tap('.tab[data-tab="B"]');
    await sleep(400);
    await setVal('#printNos', '710000000004140,710000000004141,710000000004142');
    await tap('#btnQuery');
    await sleep(700);
    await tap('#checkAllTop');
    await sleep(300);
    await tap('#btnPrint');         // 第 1 次 → alert（验证 js_dialog）
    await sleep(900);
    await tap('#btnPrint');         // 第 2 次 → 分支 B1
    await sleep(700);
    await tap('#btnPrint');         // 第 3 次 → 一串 DOM 模态框
    await sleep(600);
    for (let i = 0; i < 6; i++) {
      await tap('#modalClose');
      await sleep(350);
    }
    await tap('#btnPrint');         // 第 4 次 → 手机号输入
    await sleep(700);
    await setVal('#phoneInput', '13000000000', 'input,change');
    await tap('#modalFooter .btn');
    await sleep(700);
    console.log('phase B done');
  }

  if (PHASE === 'C') {
    await tap('.tab[data-tab="C"]');
    await sleep(400);
    for (let i = 1; i <= 2; i++) {
      await setVal('#returnMailNo', `71000000000${4145 + i}`);
      await tap('#btnReturn');
      await sleep(600);
    }
    await tap('#btnDelivered');     // 先 confirm（js_dialog），再 DOM 模态框
    await sleep(900);
    await tap('#modalFooter .btn');
    await sleep(400);
    console.log('phase C done');
  }

  if (PHASE === 'D') {
    await tap('.tab[data-tab="D"]');
    await sleep(400);
    await tap('#btnHandover');
    await sleep(800);
    console.log('phase D done');
  }

  if (PHASE === 'close') {
    await send('Browser.close');
    await sleep(500);
  }

  ws.close();
  if (PHASE !== 'nav') await sleep(200);
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error('DRIVER ERROR:', e.message);
    process.exit(1);
  }
);
