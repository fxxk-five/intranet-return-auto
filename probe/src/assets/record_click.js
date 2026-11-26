/* record_click.js —— 点击 / 输入 / 回车取证（§7.5.2）
 *
 * 由 Rust 侧通过 Page.addScriptToEvaluateOnNewDocument 注入（每个新文档都装一次），
 * 再用 Runtime.evaluate 立刻装一次（当前文档）。
 *
 * 铁律：永不抛错、永不阻止冒泡、永不改值。任何回调里的异常都必须被吞掉，
 * 否则会把操作员的页面搞坏 —— 那就等于探针自己毁了取证现场。
 */
(function () {
  if (window.__probeInstalled) return;
  window.__probeInstalled = true;

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return '';
    if (el.id) return '#' + (window.CSS && CSS.escape ? CSS.escape(el.id) : el.id);
    var parts = [];
    var cur = el, depth = 0;
    while (cur && cur.nodeType === 1 && depth < 12) {
      var sel = cur.tagName.toLowerCase();
      if (cur.getAttribute && cur.getAttribute('name')) {
        sel += "[name='" + cur.getAttribute('name') + "']";
      }
      var p = cur.parentElement;
      if (p) {
        var idx = Array.prototype.indexOf.call(p.children, cur) + 1;
        sel += ':nth-child(' + idx + ')';
      }
      parts.unshift(sel);
      cur = p; depth++;
    }
    return parts.join(' > ');
  }

  function describe(el) {
    if (!el || el.nodeType !== 1) return null;
    var r = el.getBoundingClientRect();
    return {
      tag: el.tagName.toLowerCase(),
      id: el.id || null,
      name: el.getAttribute ? el.getAttribute('name') : null,
      cls: (el.className && typeof el.className === 'string') ? el.className.slice(0, 200) : null,
      type: el.getAttribute ? el.getAttribute('type') : null,
      // 输入框的 text 绝不能用 el.value（会把单号原文带出去）；
      // 统一走 maskInPage，与 Rust 侧 redact 规则一致。
      // 文本优先级：aria-label > title > placeholder > innerText
      // （状态组件会把 on/off 塞进 innerText，把 key 污染成 B.on）
      text: (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')
        ? maskInPage(el.value)
        : ((el.getAttribute('aria-label') || el.getAttribute('title')
            || el.getAttribute('placeholder') || el.innerText || '') + '').trim().slice(0, 80),
      placeholder: el.getAttribute ? el.getAttribute('placeholder') : null,
      rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
      css: cssPath(el)
    };
  }

  function send(type, extra) {
    try {
      if (typeof window.__probe === 'function') {
        var payload = { type: type, ts: Date.now(), url: location.href };
        if (extra) for (var k in extra) if (Object.prototype.hasOwnProperty.call(extra, k)) payload[k] = extra[k];
        window.__probe(JSON.stringify(payload));
      }
    } catch (e) { /* 静默，绝不打断页面 */ }
  }

  // pointerdown 比 click 早，且此刻 DOM 还没被业务 handler 改过。
  // **同步**序列化整页 HTML 随事件一起发走 —— 这是 ui_delta 方向正确的前提：
  // 等 Rust 收到 click 再去截图，"前"图里弹窗已经出来了。
  document.addEventListener('pointerdown', function (e) {
    try {
      var t = e.target;
      var el = (t && t.closest && t.closest('button,a,input,select,textarea,[role=button],[role=tab],li')) || t;
      var html = '';
      var truncated = false;
      try {
        html = document.documentElement ? document.documentElement.outerHTML : '';
        if (html.length > 3000000) { html = html.slice(0, 3000000); truncated = true; }
      } catch (err) { html = ''; }
      send('pointer_down', {
        element: describe(el),
        ts_ms: Date.now(),
        dom_truncated: truncated,
        dom_html_len: html.length,
        dom_html: html
      });
    } catch (e2) { /* 静默 */ }
  }, true);

  document.addEventListener('click', function (e) {
    try {
      var raw = e.target;
      var el = (raw.closest && raw.closest('button,a,input,select,textarea,[role=button],[role=tab],li')) || raw;
      send('click', {
        element: describe(el),
        target_chain: [describe(el), describe(el && el.parentElement)]
      });
    } catch (e2) { /* 静默 */ }
  }, true);

  // FNV-1a 32 位：够用于"是不是同一个值"，无依赖、无碰撞风险（配合长度）
  function quickHash(str) {
    var h = 0x811c9dc5;
    for (var i = 0; i < str.length; i++) {
      h ^= str.charCodeAt(i);
      h = (h * 0x01000193) >>> 0;
    }
    return ('00000000' + h.toString(16)).slice(-8);
  }

  // 与 Rust redact::mask_mailno 完全一致：保留前 4 位，其余星号。
  // 固定向量（改这里必须同步改 src/inputs.rs 的测试）：
  //   710000000004161 -> 7100***********
  //   7100000000041   -> 7100*********
  function maskInPage(v) {
    var s = String(v || '').trim();
    if (s.length < 11 || s.length > 16 || !/^\d+$/.test(s)) return s;
    return s.slice(0, 4) + new Array(s.length - 4 + 1).join('*');
  }

  document.addEventListener('change', function (e) {
    try {
      var el = e.target;
      if (!el) return;
      var v = (el.value || '');
      var tokens = v.split(/[\s,，;；]+/).filter(function (t) { return /^\d{11,16}$/.test(t); }).slice(0, 50);
      send('change', {
        element: describe(el),
        value_len: v.length,
        value_preview: v.slice(0, 30),
        // 原始值不离开页面：只给指纹 + 脱敏值 + 单号数组
        value_hash: quickHash(v),
        value_masked: maskInPage(v),
        looks_like_mailno: /^\d{11,16}$/.test(v.trim()),
        tokens: tokens
      });
    } catch (e2) { /* 静默 */ }
  }, true);

  document.addEventListener('keydown', function (e) {
    try {
      if (e.key === 'Enter') {
        send('enter', { element: describe(e.target) });
      }
    } catch (e2) { /* 静默 */ }
  }, true);

  send('installed', {});
})();
