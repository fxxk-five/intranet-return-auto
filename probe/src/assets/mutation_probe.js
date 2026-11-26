/* mutation_probe.js —— DOM 模态框观测（§7.6 dom_modal 类）
 *
 * 只上报"新出现的、看起来像弹窗"的节点，节流 100ms，绝不修改 DOM。
 * 判定与 §7.6 表格一致：role=dialog、常见企业框架类名、position:fixed 且 z-index>1000。
 */
(function () {
  if (window.__probeMutationInstalled) return;
  window.__probeMutationInstalled = true;

  var lastSent = 0;

  function send(type, extra) {
    try {
      if (typeof window.__probe === 'function') {
        var payload = { type: type, ts: Date.now(), url: location.href };
        if (extra) for (var k in extra) if (Object.prototype.hasOwnProperty.call(extra, k)) payload[k] = extra[k];
        window.__probe(JSON.stringify(payload));
      }
    } catch (e) { /* 静默 */ }
  }

  function isDialogish(el) {
    if (!el || el.nodeType !== 1) return false;
    try {
      var role = el.getAttribute && el.getAttribute('role');
      if (role === 'dialog' || role === 'alertdialog') return true;
      var cls = (el.className && typeof el.className === 'string') ? el.className : '';
      if (/el-dialog|el-message-box|el-drawer|ant-modal|ant-message|modal|dialog|layui-layer|messager/i.test(cls)) return true;
      var st = window.getComputedStyle(el);
      if ((st.position === 'fixed' || st.position === 'absolute')
          && parseInt(st.zIndex || '0', 10) > 1000
          && el.getBoundingClientRect().width > 80) {
        return true;
      }
    } catch (e) { /* 静默 */ }
    return false;
  }

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return '';
    if (el.id) return '#' + (window.CSS && CSS.escape ? CSS.escape(el.id) : el.id);
    var parts = [], cur = el, depth = 0;
    while (cur && cur.nodeType === 1 && depth < 10) {
      var sel = cur.tagName.toLowerCase();
      var p = cur.parentElement;
      if (p) sel += ':nth-child(' + (Array.prototype.indexOf.call(p.children, cur) + 1) + ')';
      parts.unshift(sel);
      cur = p; depth++;
    }
    return parts.join(' > ');
  }

  function report(el) {
    var now = Date.now();
    if (now - lastSent < 100) return;   // 节流 100ms
    lastSent = now;
    var r = el.getBoundingClientRect();
    var st = {};
    try {
      var cs = window.getComputedStyle(el);
      st = { z: parseInt(cs.zIndex || '0', 10), pos: cs.position };
    } catch (e) { /* 静默 */ }
    send('mutation_dialog', {
      selector: cssPath(el),
      text_preview: ((el.innerText || '') + '').trim().slice(0, 120),
      rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
      z_index: st.z || null,
      role: el.getAttribute ? el.getAttribute('role') : null,
      cls: (el.className && typeof el.className === 'string') ? el.className.slice(0, 160) : null
    });
  }

  try {
    var mo = new MutationObserver(function (muts) {
      try {
        for (var i = 0; i < muts.length; i++) {
          var added = muts[i].addedNodes;
          for (var j = 0; j < added.length; j++) {
            var n = added[j];
            if (n.nodeType !== 1) continue;
            if (isDialogish(n)) { report(n); continue; }
            // 弹窗容器常整体挂载，再扫一层子节点
            var kids = n.querySelectorAll ? n.querySelectorAll('[role=dialog],.el-dialog,.el-message-box,.modal,.ant-modal') : [];
            for (var k = 0; k < kids.length; k++) { report(kids[k]); break; }
          }
        }
      } catch (e) { /* 静默 */ }
    });
    mo.observe(document.documentElement || document, { childList: true, subtree: true });
  } catch (e) { /* 静默 */ }
})();
