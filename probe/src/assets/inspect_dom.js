/* inspect_dom.js —— 元素清单 + 定位器 + 页面级信息（§7.4 / §8.2）
 *
 * 由 Rust 调：  window.__probeInspectDom({ controlTargets: [...], pageTargetCount: N, tabTargets: [...] })
 * 返回：JSON 字符串（string，不是对象 —— 避免 CDP returnByValue 的循环引用问题）
 *
 * 设计约束：
 *  - 只采集 §7.4.2 列的选择器范围，不遍历所有 *（企业页面上万节点）
 *  - 上限 3000 个元素，超出置 elements_truncated
 *  - 每个 selector 生成时就校验唯一性（不唯一就标 unique:false，不许猜）
 *  - 全程 try/catch，页面坏掉等于取证失败
 */
(function () {
  if (window.__probeInspectDom) return;

  var MAX_ELEMENTS = 3000;

  var QUERY = [
    'input', 'textarea', 'select', 'button', 'a[href]',
    '[role=button]', '[role=checkbox]', '[role=dialog]', '[onclick]',
    '[class*="btn"]', '[class*="button"]',
    'label', 'iframe', 'table', 'form',
    '[class*="el-select"]', '[class*="el-dialog"]', '[class*="el-message"]'
  ].join(',');

  function txt(el, max) {
    var t = '';
    try {
      t = (el.innerText || el.textContent || el.value || '') + '';
    } catch (e) { t = ''; }
    t = t.replace(/\s+/g, ' ').trim();
    return max ? t.slice(0, max) : t;
  }

  function attr(el, name) {
    try { return el.getAttribute ? el.getAttribute(name) : null; } catch (e) { return null; }
  }

  function isVisible(el) {
    try {
      var r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) return false;
      var st = window.getComputedStyle(el);
      if (st.display === 'none' || st.visibility === 'hidden' || parseFloat(st.opacity) === 0) return false;
      return true;
    } catch (e) { return false; }
  }

  function escapeIdent(s) {
    return (window.CSS && CSS.escape) ? CSS.escape(s) : String(s).replace(/([^\w-])/g, '\\$1');
  }

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return '';
    var parts = [], cur = el, depth = 0;
    while (cur && cur.nodeType === 1 && depth < 12) {
      var seg = cur.tagName.toLowerCase();
      if (cur.id) { parts.unshift('#' + escapeIdent(cur.id)); break; }
      var p = cur.parentElement;
      if (p) {
        var idx = Array.prototype.indexOf.call(p.children, cur) + 1;
        seg += ':nth-child(' + idx + ')';
      }
      parts.unshift(seg);
      cur = p; depth++;
    }
    return parts.join(' > ');
  }

  function xpathOf(el) {
    var parts = [];
    var cur = el;
    while (cur && cur.nodeType === 1) {
      var idx = 1, sib = cur.previousElementSibling;
      while (sib) { if (sib.tagName === cur.tagName) idx++; sib = sib.previousElementSibling; }
      parts.unshift(cur.tagName.toLowerCase() + '[' + idx + ']');
      cur = cur.parentElement;
    }
    return '/' + parts.join('/');
  }

  function cssCount(sel) {
    try { return document.querySelectorAll(sel).length; } catch (e) { return -1; }
  }

  function xpathCount(xp) {
    try {
      var r = document.evaluate(xp, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
      return r.snapshotLength;
    } catch (e) { return -1; }
  }

  function labelTextFor(el) {
    try {
      if (el.labels && el.labels.length) return txt(el.labels[0], 40);
      var id = el.id;
      if (id) {
        var l = document.querySelector('label[for="' + escapeIdent(id) + '"]');
        if (l) return txt(l, 40);
      }
      var p = el.parentElement;
      for (var i = 0; i < 3 && p; i++) {
        var lab = p.querySelector && p.querySelector('label');
        if (lab) return txt(lab, 40);
        p = p.parentElement;
      }
    } catch (e) { /* 静默 */ }
    return null;
  }

  function buildSelectors(el) {
    var out = [];

    var id = el.id;
    if (id) {
      var sel = '#' + escapeIdent(id);
      out.push({ type: 'id', value: sel, unique: cssCount(sel) === 1 });
    }

    var name = attr(el, 'name');
    if (name) {
      var s2 = el.tagName.toLowerCase() + "[name='" + name.replace(/'/g, "\\'") + "']";
      out.push({ type: 'name', value: s2, unique: cssCount(s2) === 1 });
    }

    // data-*
    try {
      var attrs = el.attributes || [];
      for (var i = 0; i < attrs.length; i++) {
        var an = attrs[i].name;
        if (an.indexOf('data-') === 0 && attrs[i].value && attrs[i].value.length < 60) {
          var s3 = el.tagName.toLowerCase() + '[' + an + "='" + String(attrs[i].value).replace(/'/g, "\\'") + "']";
          out.push({ type: 'data', value: s3, unique: cssCount(s3) === 1 });
          break;
        }
      }
    } catch (e) { /* 静默 */ }

    // 文本 / label → xpath（对中文按钮最实用）
    var t = txt(el, 40);
    if (t && t.length <= 40 && (el.tagName === 'BUTTON' || el.tagName === 'A' || attr(el, 'role') === 'button')) {
      var safe = t.replace(/"/g, '');
      var xp = "//" + el.tagName.toLowerCase() + "[normalize-space()='" + safe + "']";
      out.push({ type: 'xpath', value: xp, unique: xpathCount(xp) === 1 });
    }
    var lab = labelTextFor(el);
    if (lab) {
      var xp2 = "//label[normalize-space()='" + lab.replace(/"/g, '') + "']/following::" + el.tagName.toLowerCase() + "[1]";
      out.push({ type: 'xpath_label', value: xp2, unique: xpathCount(xp2) === 1 });
    }

    var cp = cssPath(el);
    if (cp) {
      var depth = (cp.match(/>/g) || []).length + 1;
      out.push({ type: 'css', value: cp, unique: cssCount(cp) === 1, depth: depth });
    }

    var xp3 = xpathOf(el);
    out.push({ type: 'xpath_abs', value: xp3, unique: false, note: '绝对路径，仅作兜底' });

    return out;
  }

  function fragileOf(el, selectors) {
    var flags = [];
    var cls = (el.className && typeof el.className === 'string') ? el.className : '';
    if (/(^|\s)(css|scss|jsx)-/i.test(cls)) flags.push('css_in_js_class');
    if (/[a-z]_[a-zA-Z0-9]{8,}/.test(cls)) flags.push('random_suffix_class');
    if (el.id && /\d{6,}/.test(el.id)) flags.push('random_id');
    for (var i = 0; i < selectors.length; i++) {
      var s = selectors[i];
      if (s.type === 'css' && s.depth > 8) { flags.push('css_path_too_deep'); break; }
    }
    if (/(^|\s)_[a-z0-9]{6,}/i.test(cls)) flags.push('underscore_hash_class');
    return flags;
  }

  function elementInfo(el, seq, inShadow) {
    var r = { left: 0, top: 0, width: 0, height: 0 };
    try { r = el.getBoundingClientRect(); } catch (e) { /* 静默 */ }
    var st = {};
    try { st = window.getComputedStyle(el); } catch (e) { /* 静默 */ }
    var selectors = [];
    try { selectors = buildSelectors(el); } catch (e) { selectors = []; }
    var disabled = false, readonly = false;
    try { disabled = !!el.disabled; readonly = !!el.readOnly; } catch (e) { /* 静默 */ }

    return {
      seq: seq,
      tag: el.tagName.toLowerCase(),
      text: txt(el, 80),
      placeholder: attr(el, 'placeholder'),
      aria_label: attr(el, 'aria-label'),
      label_text: labelTextFor(el),
      attrs: {
        id: el.id || null,
        name: attr(el, 'name'),
        class: (typeof el.className === 'string' ? el.className.slice(0, 200) : null),
        type: attr(el, 'type'),
        maxlength: attr(el, 'maxlength'),
        href: attr(el, 'href'),
        disabled: disabled,
        readonly: readonly
      },
      visible: isVisible(el),
      enabled: !disabled,
      rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
      z_index: (function () {
        var z = parseInt(st.zIndex || '0', 10);
        return isNaN(z) ? null : z;
      })(),
      in_shadow: !!inShadow,
      selectors: selectors,
      fragile: fragileOf(el, selectors)
    };
  }

  function collectIn(root, out, inShadow, budget) {
    var nodes;
    try { nodes = root.querySelectorAll(QUERY); } catch (e) { return; }
    for (var i = 0; i < nodes.length; i++) {
      if (out.length >= budget) return;
      out.push(elementInfo(nodes[i], out.length + 1, inShadow));
      // shadow root 递归（§7.4.1）
      try {
        if (nodes[i].shadowRoot) collectIn(nodes[i].shadowRoot, out, true, budget);
      } catch (e) { /* 静默 */ }
    }
  }

  function framesAndElements(budget) {
    var frames = [];
    var total = 0;

    var topEls = [];
    collectIn(document, topEls, false, budget);
    total += topEls.length;
    frames.push({
      frame_id: 'top',
      url: location.href,
      cross_origin: false,
      elements: topEls
    });

    // 同源 iframe 递归，最大深度 5（§7.4.1）
    var iframes = [];
    try { iframes = Array.prototype.slice.call(document.querySelectorAll('iframe')); } catch (e) { /* 静默 */ }
    for (var i = 0; i < iframes.length && total < budget; i++) {
      var f = iframes[i];
      var doc = null;
      try { doc = f.contentDocument; } catch (e) { doc = null; }
      if (!doc) {
        frames.push({
          frame_id: 'iframe-' + i,
          url: f.src || null,
          cross_origin: true,
          note: '跨域，已由 CDP Target.setAutoAttach 单独 attach',
          elements: []
        });
        continue;
      }
      var els = [];
      collectIn(doc, els, false, budget - total);
      total += els.length;
      frames.push({
        frame_id: 'iframe-' + i,
        url: (doc.location && doc.location.href) || f.src || null,
        cross_origin: false,
        elements: els
      });
      if (total >= budget) break;
    }

    return { frames: frames, total: total };
  }

  function detectFrameworks() {
    var found = [];
    try { if (window.jQuery) found.push('jQuery'); } catch (e) { /* 静默 */ }
    try { if (window.Vue) found.push('Vue'); } catch (e) { /* 静默 */ }
    try { if (window.React) found.push('React'); } catch (e) { /* 静默 */ }
    try { if (window.Ext) found.push('ExtJS'); } catch (e) { /* 静默 */ }
    try { if (window.layui) found.push('layui'); } catch (e) { /* 静默 */ }
    try { if (window.ElementUI) found.push('element-ui'); } catch (e) { /* 静默 */ }
    try { if (window.axios) found.push('axios'); } catch (e) { /* 静默 */ }
    return found;
  }

  function xUaCompatible() {
    try {
      var m = document.querySelector('meta[http-equiv="X-UA-Compatible"]');
      return m ? m.getAttribute('content') : null;
    } catch (e) { return null; }
  }

  function tabStructure(args) {
    // 1. 多个 page target
    if (args.pageTargetCount && args.pageTargetCount > 1) return { kind: 'browser_tabs', tabs: args.tabTargets || [] };

    // 2. 多个业务 iframe
    var ifr = [];
    try { ifr = Array.prototype.slice.call(document.querySelectorAll('iframe')); } catch (e) { /* 静默 */ }
    var kw = /扫描|补打|退回|妥投|交接|scan|print|return|handover/i;
    var bizIfr = ifr.filter(function (f) {
      return kw.test(f.getAttribute('src') || '') || kw.test(f.getAttribute('title') || '') || kw.test(f.getAttribute('name') || '');
    });
    if (bizIfr.length >= 4) return { kind: 'iframes', count: bizIfr.length };

    // 3. DOM 标签页容器
    var c = 0;
    try {
      var tablists = document.querySelectorAll('[role=tablist], .tabs, .tab-list, .el-tabs, .ant-tabs, [class*="tabbar"]');
      c = tablists.length;
      if (c === 0) {
        var tabs = document.querySelectorAll('.tab, [role=tab], [class*="tab-item"]');
        if (tabs.length >= 4) c = Math.ceil(tabs.length / 4);
      }
    } catch (e) { /* 静默 */ }
    if (c >= 1) return { kind: 'dom_tabs', count: c };

    return { kind: 'unknown' };
  }

  function matchControl(targets, allElements) {
    // §7.4.5：精确等于优先，其次包含；不唯一必须给 note，不许猜
    var map = {};
    for (var i = 0; i < targets.length; i++) {
      var t = targets[i];
      var hits = [];
      for (var j = 0; j < allElements.length; j++) {
        var el = allElements[j];
        if (!el.visible) continue;
        var fields = [el.text, el.placeholder, el.aria_label, el.label_text];
        var exact = false, contains = false;
        for (var k = 0; k < fields.length; k++) {
          var f = fields[k];
          if (!f) continue;
          if (f === t) exact = true;
          else if (f.indexOf(t) >= 0) contains = true;
        }
        if (exact) hits.push({ el: el, rank: 0 });
        else if (contains) hits.push({ el: el, rank: 1 });
      }
      hits.sort(function (a, b) { return a.rank - b.rank; });

      if (hits.length === 0) {
        map[t] = { selector: null, unique: false, note: '未找到匹配元素' };
        continue;
      }
      var best = hits[0].el;
      var primary = null, primaryUnique = false;
      for (var s = 0; s < best.selectors.length; s++) {
        var sel = best.selectors[s];
        if (sel.unique && (sel.type === 'id' || sel.type === 'name' || sel.type === 'xpath' || sel.type === 'xpath_label' || sel.type === 'data')) {
          primary = sel.value;
          primaryUnique = true;
          break;
        }
      }
      map[t] = {
        selector: primary,
        unique: primaryUnique,
        tag: best.tag,
        attr_maxlength: best.attrs.maxlength,
        matched_text: best.text || null,
        candidates: hits.length,
        note: primaryUnique ? null : ('命中 ' + hits.length + ' 个候选，无非唯一可用的稳定定位器，需人工确认')
      };

      // 同义词变体：把"按钮/输入框"等后缀去掉再试一次
      var variants = [];
      ['输入框', '按钮', '下拉框', '复选框', '标签页'].forEach(function (suffix) {
        if (t.length > suffix.length && t.slice(-suffix.length) === suffix) {
          variants.push(t.slice(0, t.length - suffix.length));
        }
      });
      for (var v = 0; v < variants.length; v++) {
        if (!map[variants[v]]) {
          map[variants[v]] = Object.assign({ note: '由 ' + t + ' 派生的同义变体' }, map[t]);
        }
      }
    }
    return map;
  }

  window.__probeInspectDom = function (args) {
    args = args || {};
    try {
      var collected = framesAndElements(MAX_ELEMENTS);
      var flat = [];
      collected.frames.forEach(function (f) {
        f.elements.forEach(function (e) { flat.push(e); });
      });
      var ts = tabStructure(args);
      var out = {
        generated_at: new Date().toISOString(),
        page: {
          url: location.href,
          title: document.title,
          ua: navigator.userAgent,
          is_chromium: /Edg\/|Chrome\//.test(navigator.userAgent),
          x_ua_compatible: xUaCompatible(),
          document_mode: (typeof document.documentMode === 'undefined') ? null : document.documentMode,
          frameworks: detectFrameworks(),
          has_show_modal_dialog: typeof window.showModalDialog === 'function',
          tab_structure: ts.kind,
          tab_detail: ts,
          tab_targets: args.tabTargets || [],
          elements_truncated: collected.total >= MAX_ELEMENTS
        },
        frames: collected.frames,
        control_map: matchControl(args.controlTargets || [], flat)
      };
      return JSON.stringify(out);
    } catch (e) {
      return JSON.stringify({ error: String(e && e.message || e) });
    }
  };
})();
