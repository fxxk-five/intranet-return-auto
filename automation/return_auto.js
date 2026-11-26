// ==UserScript==
// @name         内网退货自动处理助手 (A→B→A→C→D)
// @namespace    http://10.4.188.1/delivery-web/
// @version      1.0.0
// @description  内网投递系统退货全流程自动化：下段预检 → 撤单自动补打与补手机号 → 回流下段 → 退回妥投 → 归班交接
// @match        https://10.4.188.1/portal/*
// @match        http://10.4.188.1/portal/*
// @match        https://10.4.188.1/delivery-web/*
// @match        http://10.4.188.1/delivery-web/*
// @grant        none
// ==/UserScript==

(function (root, factory) {
  'use strict';
  var api = factory();
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  }
  if (typeof window !== 'undefined' && typeof document !== 'undefined') {
    root.__RETURN_AUTO__ = api;
    api.mountPanel({ startHidden: !!root.__RETURN_AUTO_EXT_MODE__ });
  }
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  var VERSION = '1.0.0';
  var STORAGE_KEY = 'intranet_return_auto_config_v1';

  var STATES = {
    PENDING: 'PENDING',
    NEED_REPRINT: 'NEED_REPRINT',
    B_PRINTED: 'B_PRINTED',
    READY_A2: 'READY_A2',
    A2_DONE: 'A2_DONE',
    C1_OK: 'C1_OK',
    C2_DONE: 'C2_DONE',
    DONE: 'DONE',
    SKIPPED: 'SKIPPED',
    FAILED: 'FAILED',
  };

  var DEFAULT_CONFIG = {
    ctx: '/delivery-web/a',
    roadseg: '',
    roadsegName: '',
    dlvPersonName: '',
    returnWorkRoadSegCode: '',
    fixedMobile: '13000000000',
    templateCode: 'SD_GN_KB_MD',
    signType: '130',
    signPersonName: '',
    batchSizeA: 50,
    batchSizeB: 50,
    batchSizeC: 50,
    batchSizeD: 50,
    delayMs: 150,
    timeoutMs: 30000,
    dryRun: true,
    runStages: { A: true, B: true, C: true, D: true },
    continueAlreadySetToC: true,
    allowZhuanduan: true,
    skipCod: true,
    skipIncompleteOneBill: true,
  };

  // =========================================================================
  // 1. 纯函数工具（输入解析 / 校验 / 模板解码 / CSV 导出）
  // =========================================================================

  function detectCtx() {
    if (typeof window !== 'undefined') {
      if (typeof window.ctx === 'string' && window.ctx.indexOf('/delivery-web') !== -1) {
        return window.ctx.replace(/\/+$/, '');
      }
      if (window.location && window.location.pathname) {
        var m = window.location.pathname.match(/^(\/delivery-web\/[^/]+)/);
        if (m) return m[1];
      }
    }
    return DEFAULT_CONFIG.ctx;
  }

  function formatDateYMD(date) {
    var d = date instanceof Date ? date : new Date();
    var y = d.getFullYear();
    var m = String(d.getMonth() + 1).padStart(2, '0');
    var day = String(d.getDate()).padStart(2, '0');
    return y + '-' + m + '-' + day;
  }

  function formatTimeHMS(date) {
    var d = date instanceof Date ? date : new Date();
    return (
      String(d.getHours()).padStart(2, '0') +
      ':' +
      String(d.getMinutes()).padStart(2, '0') +
      ':' +
      String(d.getSeconds()).padStart(2, '0')
    );
  }

  function chunkArray(arr, size) {
    var s = Math.max(1, parseInt(size, 10) || 50);
    var out = [];
    for (var i = 0; i < arr.length; i += s) {
      out.push(arr.slice(i, i + s));
    }
    return out;
  }

  function normalizeCell(cell) {
    var s = String(cell == null ? '' : cell)
      .replace(/^\uFEFF/, '')
      .trim();
    // 先解包标准 RFC-4180 CSV 双引号转义（如 "=""9841...""" -> ="9841..."）
    if (s.length >= 2 && s.charAt(0) === '"' && s.charAt(s.length - 1) === '"') {
      s = s.slice(1, -1).replace(/""/g, '"').trim();
    }
    // 兼容 Excel 文本公式 ="9841..." 或前导单引号/制表符
    var eqMatch = s.match(/^=\s*"([^"]+)"$/);
    if (eqMatch) s = eqMatch[1].trim();
    s = s.replace(/^["'\t]+|["'\t]+$/g, '').trim();
    return s;
  }

  function isLikelyWaybill(token) {
    if (!token) return false;
    if (token.length < 8 || token.length > 30) return false;
    if (!/^[A-Za-z0-9]+$/.test(token)) return false;
    var digits = token.replace(/[^0-9]/g, '');
    return digits.length >= 6;
  }

  function parseWaybillInput(rawText) {
    var text = String(rawText == null ? '' : rawText).replace(/^\uFEFF/, '');
    var lines = text.split(/\r?\n/);
    var seen = Object.create(null);
    var waybills = [];
    var duplicateCount = 0;
    var invalidSamples = [];
    var invalidCount = 0;

    for (var i = 0; i < lines.length; i++) {
      var line = lines[i].trim();
      if (!line) continue;
      // 支持逗号/分号/制表符/空白分隔
      var cells = line.split(/[,;\t\s]+/);
      var foundInLine = [];
      for (var j = 0; j < cells.length; j++) {
        var tok = normalizeCell(cells[j]);
        if (!tok) continue;
        if (isLikelyWaybill(tok)) {
          foundInLine.push(tok);
        }
      }
      if (foundInLine.length === 0) {
        // 可能是表头或无效行
        invalidCount++;
        if (invalidSamples.length < 5) invalidSamples.push(line.slice(0, 40));
        continue;
      }
      // 如果整行是多列 CSV（含非单号文字+一个单号），foundInLine 恰好只含单号；
      // 如果一行粘贴了多个单号，也会全部提取
      for (var k = 0; k < foundInLine.length; k++) {
        var wb = foundInLine[k];
        if (seen[wb]) {
          duplicateCount++;
        } else {
          seen[wb] = true;
          waybills.push(wb);
        }
      }
    }

    return {
      waybills: waybills,
      duplicateCount: duplicateCount,
      invalidCount: invalidCount,
      invalidSamples: invalidSamples,
    };
  }

  // 与 waybillprint/main.js L1782-1788、L2277-2283、L2460-2466 对齐，且对全段脱敏 * 更严格
  function needsMobileUpdate(item) {
    var mobile = String((item && item.receiverMobile) || '').trim();
    if (!mobile) return true;
    if (mobile.indexOf('*') !== -1) return true;
    var first = mobile.charAt(0);
    if (first !== '1' && first !== '0') return true;
    if (first === '1' && mobile.length !== 11) return true;
    return false;
  }

  function isValidMobile(mobile) {
    return /^1\d{10}$/.test(String(mobile || '').trim());
  }

  // modelContent 接口实际返回 JSON 编码的字符串（外层双引号 + 转义 \"），
  // 而内部 &quot; 在前端原样传给 print3。
  function decodeModelContent(raw) {
    if (raw == null) return '';
    if (typeof raw !== 'string') return String(raw);
    var trimmed = raw.trim();
    if (
      trimmed.length >= 2 &&
      trimmed.charAt(0) === '"' &&
      trimmed.charAt(trimmed.length - 1) === '"'
    ) {
      try {
        var parsed = JSON.parse(trimmed);
        if (typeof parsed === 'string') return parsed;
      } catch (e) {
        // fall through to raw string
      }
    }
    return raw;
  }

  function csvEscape(val) {
    var s = String(val == null ? '' : val);
    if (/[",\r\n]/.test(s)) {
      return '"' + s.replace(/"/g, '""') + '"';
    }
    return s;
  }

  function toCsv(records) {
    var headers = [
      '运单号',
      '最终状态',
      '停留阶段',
      '是否撤单补打',
      '是否补录手机号',
      '使用模板',
      '备注提示',
      '错误码',
      '错误信息',
      'A1预检时间',
      'B补打时间',
      'A2下段时间',
      'C妥投时间',
      'D归班时间',
    ];
    var lines = [headers.map(csvEscape).join(',')];
    for (var i = 0; i < records.length; i++) {
      var r = records[i];
      var row = [
        '="' + String(r.waybillNo || '') + '"',
        r.status || '',
        r.stage || '',
        r.needReprint ? '是' : '否',
        r.mobileUpdated ? '是' : '否',
        r.templateCode || '',
        r.notice || '',
        r.errorCode || '',
        r.errorMsg || '',
        r.a1Time || '',
        r.bTime || '',
        r.a2Time || '',
        r.cTime || '',
        r.dTime || '',
      ];
      lines.push(row.map(csvEscape).join(','));
    }
    return '\uFEFF' + lines.join('\r\n');
  }

  function encodeForm(params) {
    var pairs = [];
    var keys = Object.keys(params || {});
    for (var i = 0; i < keys.length; i++) {
      var k = keys[i];
      var v = params[k];
      if (v === undefined || v === null) v = '';
      pairs.push(
        encodeURIComponent(k) +
          '=' +
          encodeURIComponent(String(v)).replace(/%20/g, '+')
      );
    }
    return pairs.join('&');
  }

  function sleep(ms) {
    return new Promise(function (resolve) {
      setTimeout(resolve, Math.max(0, ms || 0));
    });
  }

  // =========================================================================
  // 2. 同源 XHR 传输层（兼容瑞数 WAF 对 XMLHttpRequest 的自动签名 Hook）
  // =========================================================================

  function SessionExpiredError(message) {
    this.name = 'SessionExpiredError';
    this.message = message || '登录会话已失效，请刷新内网页面重新登录后继续';
  }
  SessionExpiredError.prototype = Object.create(Error.prototype);
  SessionExpiredError.prototype.constructor = SessionExpiredError;

  function looksLikeLoginPage(text) {
    if (typeof text !== 'string') return false;
    var s = text.slice(0, 1200).toLowerCase();
    if (s.indexOf('<html') !== -1 || s.indexOf('<!doctype') !== -1) {
      if (
        s.indexOf('login') !== -1 ||
        s.indexOf('登录') !== -1 ||
        s.indexOf('password') !== -1 ||
        s.indexOf('j_username') !== -1
      ) {
        return true;
      }
    }
    return false;
  }

  function parseAttr(tagStr, attrName) {
    var re = new RegExp(attrName + '\\s*=\\s*(?:"([^"]*)"|\'([^\']*)\'|([^\\s>]+))', 'i');
    var m = String(tagStr || '').match(re);
    return m ? (m[1] !== undefined ? m[1] : m[2] !== undefined ? m[2] : m[3] || '') : '';
  }

  function parseSetRoadSegHtml(html) {
    var src = String(html == null ? '' : html);
    var res = {
      segtype: '',
      roadseg: '',
      roadsegName: '',
      roadsegs: [],
      staffId: '',
      staffName: '',
      dlvPersonName: '',
    };

    var segtypeTag = src.match(/<input\b[^>]*\bid\s*=\s*["']segtype["'][^>]*>/i);
    if (segtypeTag) res.segtype = parseAttr(segtypeTag[0], 'value').trim();

    var codeTag = src.match(/<input\b[^>]*\bid\s*=\s*["']roadsegcode["'][^>]*>/i);
    var nameTag = src.match(/<input\b[^>]*\bid\s*=\s*["']roadsegname["'][^>]*>/i);
    if (codeTag) res.roadseg = parseAttr(codeTag[0], 'value').trim();
    if (nameTag) res.roadsegName = parseAttr(nameTag[0], 'value').trim();

    // 多道段账号：<select id="dlvroadinfo"><option value="T01&&T01">...
    var selectMatch = src.match(/<select\b[^>]*\bid\s*=\s*["']dlvroadinfo["'][^>]*>([\s\S]*?)<\/select>/i);
    if (selectMatch) {
      var optRe = /<option\b([^>]*)>([\s\S]*?)<\/option>/gi;
      var om;
      var selectedSeg = null;
      while ((om = optRe.exec(selectMatch[1])) !== null) {
        var attrs = om[1] || '';
        var val = parseAttr('<option ' + attrs + '>', 'value').trim();
        if (!val || val === 'isblank') continue;
        var parts = val.split('&&');
        var code = (parts[0] || '').trim();
        var name = (parts[1] || code).trim();
        if (!code) continue;
        var item = { code: code, name: name };
        res.roadsegs.push(item);
        if (!selectedSeg && /\bselected\b/i.test(attrs)) selectedSeg = item;
      }
      if ((!res.roadseg || res.segtype === 'multi') && res.roadsegs.length > 0) {
        var chosen = selectedSeg || res.roadsegs[0];
        res.roadseg = chosen.code;
        res.roadsegName = chosen.name;
      }
    }
    if (res.roadseg && res.roadsegs.length === 0) {
      res.roadsegs.push({ code: res.roadseg, name: res.roadsegName || res.roadseg });
    }
    if (!res.roadsegName && res.roadseg) res.roadsegName = res.roadseg;

    // 解析当前登录投递员：<div id="singlestaff">...投递员: <input id="staff" type="hidden">710000000004161&nbsp; 张三</input>...
    var staffBlock = src.match(/<div\b[^>]*\bid\s*=\s*["']singlestaff["'][^>]*>([\s\S]*?)<\/div>/i);
    if (staffBlock) {
      var plain = staffBlock[1]
        .replace(/<[^>]+>/g, ' ')
        .replace(/&nbsp;/gi, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      var m1 = plain.match(/投递员\s*[:：]?\s*(\d{5,})\s+([^\s]+)/);
      var m2 = !m1 && plain.match(/投递员\s*[:：]?\s*([^\s]+)\s+(\d{5,})/);
      if (m1) {
        res.staffId = m1[1];
        res.staffName = m1[2];
        res.dlvPersonName = m1[2] + '-' + m1[1];
      } else if (m2) {
        res.staffName = m2[1];
        res.staffId = m2[2];
        res.dlvPersonName = m2[1] + '-' + m2[2];
      }
    }

    return res;
  }

  function readAccountFromDom() {
    if (typeof document === 'undefined') return null;
    var docs = [document];
    try {
      var iframes = document.querySelectorAll('iframe');
      for (var i = 0; i < iframes.length; i++) {
        try {
          if (iframes[i].contentDocument) docs.push(iframes[i].contentDocument);
        } catch (e) {}
      }
    } catch (e) {}
    for (var d = 0; d < docs.length; d++) {
      var doc = docs[d];
      if (!doc) continue;
      if (doc.getElementById('roadsegcode') || doc.getElementById('dlvroadinfo')) {
        var html = doc.documentElement ? doc.documentElement.innerHTML : '';
        var parsed = parseSetRoadSegHtml(html);
        if (parsed.roadseg || parsed.dlvPersonName) return parsed;
      }
    }
    return null;
  }

  function getPreferredXhrCtor() {
    // 若当前在 /portal/a 顶层页且内部已打开同源 /delivery-web/ iframe，优先借用其 XHR（保证携带业务域瑞数 Hook），否则用顶层 window.XMLHttpRequest（同样带有瑞数 Hook）
    try {
      if (typeof document !== 'undefined') {
        var iframes = document.querySelectorAll('iframe');
        for (var i = 0; i < iframes.length; i++) {
          try {
            var cw = iframes[i].contentWindow;
            if (
              cw &&
              cw.location &&
              String(cw.location.pathname || '').indexOf('/delivery-web') === 0 &&
              typeof cw.XMLHttpRequest === 'function'
            ) {
              return cw.XMLHttpRequest;
            }
          } catch (e) {}
        }
      }
    } catch (e) {}
    return typeof XMLHttpRequest !== 'undefined' ? XMLHttpRequest : null;
  }

  function createXhrTransport(defaultTimeoutMs) {
    return function xhrRequest(url, params, options) {
      var opts = options || {};
      var method = String(opts.method || 'POST').toUpperCase();
      var expectText = !!opts.expectText;
      var timeoutMs = opts.timeoutMs || defaultTimeoutMs || 30000;

      return new Promise(function (resolve, reject) {
        var XhrCtor = getPreferredXhrCtor();
        if (!XhrCtor) {
          reject(new Error('当前环境不存在 XMLHttpRequest，请注入自定义 transport'));
          return;
        }
        var xhr = new XhrCtor();
        xhr.open(method, url, true);
        xhr.withCredentials = true;
        xhr.timeout = timeoutMs;
        if (method !== 'GET') {
          xhr.setRequestHeader(
            'Content-Type',
            'application/x-www-form-urlencoded; charset=UTF-8'
          );
          xhr.setRequestHeader('X-Requested-With', 'XMLHttpRequest');
        }
        xhr.setRequestHeader(
          'Accept',
          expectText
            ? 'text/html, */*; q=0.01'
            : 'application/json, text/javascript, */*; q=0.01'
        );

        xhr.onload = function () {
          var status = xhr.status;
          var body = xhr.responseText || '';
          if (status === 401 || status === 403 || status === 302) {
            reject(
              new SessionExpiredError(
                'HTTP ' + status + '：登录态已过期或无权限，请刷新页面重新登录'
              )
            );
            return;
          }
          if (status < 200 || status >= 300) {
            reject(new Error('HTTP ' + status + ' 请求失败: ' + url));
            return;
          }
          if (looksLikeLoginPage(body)) {
            reject(
              new SessionExpiredError(
                '服务端返回登录页面，登录会话已失效，请重新登录'
              )
            );
            return;
          }
          if (expectText) {
            resolve(body);
            return;
          }
          if (!body.trim()) {
            resolve({});
            return;
          }
          try {
            resolve(JSON.parse(body));
          } catch (err) {
            reject(
              new Error(
                '响应非合法 JSON (' +
                  url +
                  '): ' +
                  body.slice(0, 120).replace(/\s+/g, ' ')
              )
            );
          }
        };

        xhr.onerror = function () {
          reject(new Error('网络异常或连接中断: ' + url));
        };
        xhr.ontimeout = function () {
          reject(new Error('请求超时 (' + timeoutMs + 'ms): ' + url));
        };

        xhr.send(method === 'GET' ? null : encodeForm(params));
      });
    };
  }

  // =========================================================================
  // 3. 核心状态机引擎：A1(预检) → B(补打+补手机号) → A1重扫 → A2(确认下段) → C(退回妥投) → D(归班交接)
  // =========================================================================

  function mergeConfig(userConfig) {
    var u = userConfig || {};
    var cfg = Object.assign({}, DEFAULT_CONFIG, u);
    cfg.ctx = (u.ctx || detectCtx() || DEFAULT_CONFIG.ctx).replace(/\/+$/, '');
    cfg.roadseg = String(cfg.roadseg || '').trim();
    cfg.roadsegName = String(cfg.roadsegName || cfg.roadseg || '').trim();
    cfg.dlvPersonName = String(cfg.dlvPersonName || '').trim();
    cfg.returnWorkRoadSegCode = String(cfg.returnWorkRoadSegCode || '').trim();
    cfg.fixedMobile = String(cfg.fixedMobile || '').trim();
    cfg.templateCode = String(cfg.templateCode || 'SD_GN_KB_MD').trim();
    cfg.signType = String(cfg.signType || '130').trim();
    cfg.signPersonName = String(cfg.signPersonName || '').trim();
    cfg.batchSizeA = Math.min(200, Math.max(1, parseInt(cfg.batchSizeA, 10) || 50));
    cfg.batchSizeB = Math.min(100, Math.max(1, parseInt(cfg.batchSizeB, 10) || 50));
    cfg.batchSizeC = Math.min(200, Math.max(1, parseInt(cfg.batchSizeC, 10) || 50));
    cfg.batchSizeD = Math.min(200, Math.max(1, parseInt(cfg.batchSizeD, 10) || 50));
    cfg.delayMs = Math.max(0, parseInt(cfg.delayMs, 10) || 0);
    cfg.timeoutMs = Math.max(3000, parseInt(cfg.timeoutMs, 10) || 30000);
    cfg.dryRun = cfg.dryRun !== false;
    cfg.runStages = Object.assign(
      { A: true, B: true, C: true, D: true },
      u.runStages || {}
    );
    return cfg;
  }

  function appendNotice(rec, text) {
    if (!text) return;
    rec.notice = rec.notice ? rec.notice + '；' + text : text;
  }

  function ReturnAutoEngine(userConfig, hooks) {
    this.config = mergeConfig(userConfig);
    this.hooks = hooks || {};
    this.transport =
      this.hooks.transport || createXhrTransport(this.config.timeoutMs);
    this.nowFn = this.hooks.nowFn || function () { return new Date(); };

    this.records = [];
    this.recordMap = Object.create(null);
    this.templateListCache = Object.create(null);
    this.templateContentCache = Object.create(null);

    this.running = false;
    this.paused = false;
    this.stopped = false;
    this._pauseResolvers = [];
  }

  ReturnAutoEngine.prototype.url = function (subPath) {
    return this.config.ctx + '/delivery/' + subPath.replace(/^\/+/, '');
  };

  ReturnAutoEngine.prototype.log = function (level, stage, message) {
    var entry = {
      time: formatTimeHMS(this.nowFn()),
      level: level || 'info',
      stage: stage || 'SYS',
      message: String(message || ''),
    };
    if (typeof this.hooks.onLog === 'function') {
      try {
        this.hooks.onLog(entry);
      } catch (e) {}
    }
  };

  ReturnAutoEngine.prototype.emitProgress = function (phaseLabel, current, total) {
    if (typeof this.hooks.onProgress === 'function') {
      try {
        this.hooks.onProgress({
          phaseLabel: phaseLabel || '',
          current: current || 0,
          total: total || this.records.length,
          summary: this.getSummary(),
        });
      } catch (e) {}
    }
  };

  ReturnAutoEngine.prototype.emitState = function (state) {
    if (typeof this.hooks.onStateChange === 'function') {
      try {
        this.hooks.onStateChange(state, this.getSummary());
      } catch (e) {}
    }
  };

  ReturnAutoEngine.prototype.pause = function () {
    if (!this.running || this.paused) return;
    this.paused = true;
    this.log('warn', 'SYS', '任务已暂停，点击“继续”可恢复执行');
    this.emitState('paused');
  };

  ReturnAutoEngine.prototype.resume = function () {
    if (!this.paused) return;
    this.paused = false;
    var list = this._pauseResolvers.splice(0, this._pauseResolvers.length);
    for (var i = 0; i < list.length; i++) list[i]();
    this.log('info', 'SYS', '任务已恢复执行');
    this.emitState('running');
  };

  ReturnAutoEngine.prototype.stop = function () {
    this.stopped = true;
    if (this.paused) {
      this.paused = false;
      var list = this._pauseResolvers.splice(0, this._pauseResolvers.length);
      for (var i = 0; i < list.length; i++) list[i]();
    }
    this.log('warn', 'SYS', '已收到中止指令，正在安全退出当前步骤...');
  };

  ReturnAutoEngine.prototype.checkpoint = async function () {
    if (this.stopped) {
      var stopErr = new Error('USER_STOPPED');
      stopErr.isStop = true;
      throw stopErr;
    }
    if (this.paused) {
      var self = this;
      await new Promise(function (resolve) {
        self._pauseResolvers.push(resolve);
      });
    }
    if (this.stopped) {
      var stopErr2 = new Error('USER_STOPPED');
      stopErr2.isStop = true;
      throw stopErr2;
    }
  };

  ReturnAutoEngine.prototype.request = async function (subPath, params, opts) {
    var options = opts || {};
    var retries = options.retries || 0;
    var method = String(options.method || 'POST').toUpperCase();
    var fullUrl = this.url(subPath);
    var lastErr = null;
    for (var attempt = 0; attempt <= retries; attempt++) {
      await this.checkpoint();
      try {
        return await this.transport(fullUrl, method === 'GET' ? null : params || {}, {
          method: method,
          expectText: !!options.expectText,
          timeoutMs: options.timeoutMs || this.config.timeoutMs,
        });
      } catch (err) {
        lastErr = err;
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) {
          throw err;
        }
        if (attempt < retries) {
          this.log(
            'warn',
            options.stage || 'NET',
            subPath + ' 请求波动，第 ' + (attempt + 1) + ' 次重试: ' + err.message
          );
          await sleep(400 * (attempt + 1));
        }
      }
    }
    throw lastErr;
  };

  ReturnAutoEngine.prototype.post = function (subPath, params, opts) {
    return this.request(subPath, params, Object.assign({ method: 'POST' }, opts || {}));
  };

  ReturnAutoEngine.prototype.detectCurrentAccount = async function () {
    var fromDom = readAccountFromDom();
    if (fromDom && (fromDom.roadseg || fromDom.dlvPersonName)) {
      fromDom.source = 'dom';
      return fromDom;
    }
    var html = await this.request('setroadseg/main', null, {
      method: 'GET',
      expectText: true,
      retries: 1,
      stage: 'INIT',
    });
    var parsed = parseSetRoadSegHtml(html);
    parsed.source = 'setroadseg/main';
    return parsed;
  };

  ReturnAutoEngine.prototype.fetchRoadSegs = async function () {
    var resp = await this.post('returnwork/getroadsegno', {}, { retries: 1, stage: 'INIT' });
    if (!Array.isArray(resp)) return [];
    var out = [];
    for (var i = 0; i < resp.length; i++) {
      var code = String((resp[i] && resp[i].dlvRoadSeg) || '').trim();
      var name = String((resp[i] && resp[i].dlvRoadsegName) || code).trim();
      if (code) out.push({ code: code, name: name });
    }
    return out;
  };

  ReturnAutoEngine.prototype.fetchCouriers = async function () {
    var resp = await this.post(
      'returnwork/queryname',
      { _datetime: String(this.nowFn().getTime()) },
      { retries: 1, stage: 'INIT' }
    );
    if (resp && Array.isArray(resp.text)) {
      return resp.text.filter(function (x) {
        return typeof x === 'string' && x.trim().length > 0;
      });
    }
    return [];
  };

  ReturnAutoEngine.prototype.getSummary = function () {
    var s = {
      total: this.records.length,
      done: 0,
      reprinted: 0,
      mobileUpdated: 0,
      skipped: 0,
      failed: 0,
      inProgress: 0,
    };
    for (var i = 0; i < this.records.length; i++) {
      var r = this.records[i];
      if (r.needReprint) s.reprinted++;
      if (r.mobileUpdated) s.mobileUpdated++;
      if (r.status === STATES.DONE) s.done++;
      else if (r.status === STATES.SKIPPED) s.skipped++;
      else if (r.status === STATES.FAILED) s.failed++;
      else s.inProgress++;
    }
    return s;
  };

  ReturnAutoEngine.prototype.validateConfig = function (waybills) {
    var errs = [];
    var c = this.config;
    if (!Array.isArray(waybills) || waybills.length === 0) {
      errs.push('请先输入或导入至少 1 个有效运单号');
    }
    if (c.runStages.B && !isValidMobile(c.fixedMobile)) {
      errs.push('启用 B 阶段（补打）时，固定手机号必须为 1 开头的 11 位数字');
    }
    if (!c.runStages.A && !c.runStages.B && !c.runStages.C && !c.runStages.D) {
      errs.push('至少勾选一个执行阶段（A / B / C / D）');
    }
    return errs;
  };

  ReturnAutoEngine.prototype._initRecords = function (waybills) {
    this.records = [];
    this.recordMap = Object.create(null);
    for (var i = 0; i < waybills.length; i++) {
      var wb = waybills[i];
      var rec = {
        waybillNo: wb,
        status: STATES.PENDING,
        stage: 'INIT',
        needReprint: false,
        mobileUpdated: false,
        roadsegObj: null,
        bizProductNo: '',
        templateCode: '',
        notice: '',
        errorCode: '',
        errorMsg: '',
        a1Time: '',
        bTime: '',
        a2Time: '',
        cTime: '',
        dTime: '',
      };
      this.records.push(rec);
      this.recordMap[wb] = rec;
    }
  };

  ReturnAutoEngine.prototype._markFailed = function (rec, stage, code, msg) {
    rec.status = STATES.FAILED;
    rec.stage = stage;
    rec.errorCode = String(code || '');
    rec.errorMsg = String(msg || '未知错误');
  };

  ReturnAutoEngine.prototype._markSkipped = function (rec, stage, code, msg) {
    rec.status = STATES.SKIPPED;
    rec.stage = stage;
    rec.errorCode = String(code || '');
    rec.errorMsg = String(msg || '已跳过');
  };

  ReturnAutoEngine.prototype._buildA1Params = function (waybillNo) {
    return {
      namelist: '-请选择-',
      alarmFlag: '0',
      waybillNo: waybillNo,
      type: '2',
      roadseg: this.config.roadseg,
      roadsegname: this.config.roadsegName || this.config.roadseg,
      societyFlag: '',
      position: '2',
      tongqian: '',
      addValueFlag: '0',
      stackFlag: '0',
    };
  };

  ReturnAutoEngine.prototype._classifyA1 = function (rec, resp, isRetryAfterB) {
    var stage = isRetryAfterB ? 'A1_RETRY' : 'A1';
    rec.stage = stage;
    rec.a1Time = formatTimeHMS(this.nowFn());

    if (!resp || typeof resp !== 'object') {
      this._markFailed(rec, stage, 'EMPTY', 'queryiteminfo 返回为空');
      return;
    }

    if (resp.resCode === 'B00001') {
      var msg = String(resp.resMsg || resp.resMess || '').trim();
      var comment = String(resp.comment || '').trim();

      if (!isRetryAfterB && (msg.indexOf('详情单补打') !== -1 || msg.indexOf('撤单') !== -1)) {
        rec.needReprint = true;
        rec.status = STATES.NEED_REPRINT;
        appendNotice(rec, '撤单邮件，路由至B阶段补打');
        return;
      }
      if (comment === 'noPostingInfo' || msg.indexOf('无收寄信息') !== -1) {
        this._markSkipped(rec, stage, 'noPostingInfo', msg || '该邮件无收寄信息，不可投递作业');
        return;
      }
      if (comment === 'itemNotExist') {
        this._markSkipped(rec, stage, 'itemNotExist', msg || '投递库无信息，需接收补录');
        return;
      }
      if (comment === 'examineInfo') {
        this._markSkipped(rec, stage, 'examineInfo', msg || '稽核邮件，需称重拍照');
        return;
      }
      if (comment === 'F') {
        this._markSkipped(rec, stage, 'F', msg || '系统红字警告拦截');
        return;
      }
      if (/已下段|已经下段|重复下段|已在道段/.test(msg)) {
        if (this.config.continueAlreadySetToC) {
          rec.status = STATES.A2_DONE;
          appendNotice(rec, '已下段直接进C阶段(' + msg + ')');
        } else {
          this._markSkipped(rec, stage, 'ALREADY_SET', msg);
        }
        return;
      }
      this._markFailed(
        rec,
        stage,
        'B00001',
        (isRetryAfterB ? '补打后重扫仍失败：' : '') + (msg || '下段预检未通过')
      );
      return;
    }

    if (!resp.waybillNo) {
      this._markFailed(rec, stage, 'NO_WAYBILL', '返回数据缺少 waybillNo 字段');
      return;
    }

    if (
      this.config.skipCod &&
      (Number(resp.codAmount || 0) > 0 || String(resp.ifPay) === '1')
    ) {
      this._markSkipped(
        rec,
        stage,
        'COD',
        '代收货款件(金额:' + (resp.codAmount || '') + ')，按安全策略跳过'
      );
      return;
    }

    if (resp.innerHtml === 'zhuanduan') {
      if (!this.config.allowZhuanduan) {
        this._markSkipped(rec, stage, 'ZHUANDUAN', resp.resMsg || '转段件已配置跳过');
        return;
      }
      appendNotice(rec, '转段件');
    }

    if (String(resp.resMsg) === '30') {
      appendNotice(
        rec,
        '改址件' + (resp.receiverAddr ? '(' + String(resp.receiverAddr).slice(0, 30) + ')' : '')
      );
    }

    rec.roadsegObj = resp;
    rec.status = STATES.READY_A2;
  };

  ReturnAutoEngine.prototype._runStageA1 = async function (targets, isRetryAfterB) {
    var stageTag = isRetryAfterB ? 'A1重扫' : 'A1预检';
    this.log('info', stageTag, '开始扫描 ' + targets.length + ' 单（道段 ' + this.config.roadseg + '）');
    for (var i = 0; i < targets.length; i++) {
      var rec = targets[i];
      await this.checkpoint();
      try {
        var resp = await this.post(
          'setroadseg/queryiteminfo',
          this._buildA1Params(rec.waybillNo),
          { retries: 2, stage: stageTag }
        );
        this._classifyA1(rec, resp, isRetryAfterB);
        if (rec.status === STATES.NEED_REPRINT) {
          this.log('info', stageTag, rec.waybillNo + ' -> 识别为客户撤单件，进入 B 阶段补打队列');
        } else if (rec.status === STATES.READY_A2) {
          this.log('ok', stageTag, rec.waybillNo + ' -> 预检通过，待确认下段' + (rec.notice ? ' [' + rec.notice + ']' : ''));
        } else if (rec.status === STATES.A2_DONE) {
          this.log('warn', stageTag, rec.waybillNo + ' -> ' + rec.notice);
        } else if (rec.status === STATES.SKIPPED) {
          this.log('warn', stageTag, rec.waybillNo + ' -> 跳过：' + rec.errorMsg);
        } else {
          this.log('error', stageTag, rec.waybillNo + ' -> 失败：' + rec.errorMsg);
        }
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        this._markFailed(rec, isRetryAfterB ? 'A1_RETRY' : 'A1', 'NET_ERR', err.message);
        this.log('error', stageTag, rec.waybillNo + ' -> 异常：' + err.message);
      }
      this.emitProgress(stageTag, i + 1, targets.length);
      if (this.config.delayMs > 0 && i < targets.length - 1) {
        await sleep(this.config.delayMs);
      }
    }
  };

  ReturnAutoEngine.prototype._resolveTemplateCode = async function (bizProductNo) {
    var configured = this.config.templateCode;
    if (configured && configured.toUpperCase() !== 'AUTO') {
      return configured;
    }
    var key = String(bizProductNo || '');
    if (!key) return DEFAULT_CONFIG.templateCode;
    if (this.templateListCache[key]) return this.templateListCache[key];

    var list = await this.post(
      'waybillprint/queryModel',
      { bizProductNo: key },
      { retries: 2, stage: 'B3' }
    );
    var chosen = '';
    if (Array.isArray(list) && list.length > 0) {
      for (var i = 0; i < list.length; i++) {
        if (list[i] && list[i].templateCode === DEFAULT_CONFIG.templateCode) {
          chosen = list[i].templateCode;
          break;
        }
      }
      if (!chosen && list[0] && list[0].templateCode) {
        chosen = String(list[0].templateCode);
      }
    }
    if (chosen) this.templateListCache[key] = chosen;
    return chosen;
  };

  ReturnAutoEngine.prototype._getModelContent = async function (templateCode) {
    if (this.templateContentCache[templateCode]) {
      return this.templateContentCache[templateCode];
    }
    var raw = await this.post(
      'waybillprint/modelContent',
      { templateCode: templateCode },
      { retries: 2, expectText: true, stage: 'B3' }
    );
    var decoded = decodeModelContent(raw);
    if (decoded) {
      this.templateContentCache[templateCode] = decoded;
    }
    return decoded;
  };

  ReturnAutoEngine.prototype._runStageB = async function (targets) {
    if (!targets.length) return;
    this.log('info', 'B补打', '开始处理 ' + targets.length + ' 单（每批 ' + this.config.batchSizeB + ' 单）');
    var batches = chunkArray(targets, this.config.batchSizeB);
    var processed = 0;

    for (var b = 0; b < batches.length; b++) {
      var batch = batches[b];
      await this.checkpoint();
      var wbListStr = batch.map(function (r) { return r.waybillNo; }).join(',');

      // B1. 补打查询（只读，演练与正式均执行）
      var qResp;
      try {
        qResp = await this.post(
          'waybillprint/query',
          { waybillNoList: wbListStr, startTime: '' },
          { retries: 2, stage: 'B1' }
        );
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        for (var i0 = 0; i0 < batch.length; i0++) {
          this._markFailed(batch[i0], 'B1', 'NET_ERR', '补打查询失败: ' + err.message);
        }
        processed += batch.length;
        this.emitProgress('B补打', processed, targets.length);
        continue;
      }

      if (!qResp || qResp.resCode === 'B00001') {
        var qErr = (qResp && (qResp.resMess || qResp.comment)) || '补打查询返回失败';
        for (var i1 = 0; i1 < batch.length; i1++) {
          this._markFailed(batch[i1], 'B1', 'B00001', qErr);
        }
        this.log('error', 'B1', '批次 ' + (b + 1) + ' 补打查询失败：' + qErr);
        processed += batch.length;
        this.emitProgress('B补打', processed, targets.length);
        continue;
      }

      var detailMap = Object.create(null);
      var details = Array.isArray(qResp.detail) ? qResp.detail : [];
      for (var d = 0; d < details.length; d++) {
        if (details[d] && details[d].waybillNo) {
          detailMap[details[d].waybillNo] = details[d];
        }
      }

      // B2. 校验并补录固定手机号 + B3. 解析打印模板
      var byTemplate = Object.create(null);
      for (var i = 0; i < batch.length; i++) {
        var rec = batch[i];
        var item = detailMap[rec.waybillNo];
        if (!item) {
          this._markFailed(
            rec,
            'B1',
            'NOT_FOUND',
            (qResp.comment && String(qResp.comment)) || '补打查询未返回该邮件'
          );
          this.log('warn', 'B1', rec.waybillNo + ' -> 未在补打查询结果中返回');
          continue;
        }
        rec.bizProductNo = String(item.bizProductNo || '');

        // 缺号或脱敏手机号自动补录
        if (needsMobileUpdate(item)) {
          await this.checkpoint();
          if (this.config.dryRun) {
            rec.mobileUpdated = true;
            this.log(
              'info',
              'B2',
              '[演练] ' +
                rec.waybillNo +
                ' 原手机号(' +
                (item.receiverMobile || '空') +
                ')不合规，将补录为 ' +
                this.config.fixedMobile
            );
          } else {
            try {
              var mResp = await this.post(
                'waybillprint/updatemobile',
                {
                  receiverMobile: this.config.fixedMobile,
                  receiverFixtel: '',
                  waybillNo: rec.waybillNo,
                },
                { retries: 0, stage: 'B2' }
              );
              if (mResp && mResp.errorMsg) {
                this._markFailed(rec, 'B2', 'UPDATE_MOBILE', mResp.errorMsg);
                this.log('error', 'B2', rec.waybillNo + ' -> 补手机号失败：' + mResp.errorMsg);
                continue;
              }
              rec.mobileUpdated = true;
              this.log('ok', 'B2', rec.waybillNo + ' -> 已补录固定手机号 ' + this.config.fixedMobile);
            } catch (err) {
              if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
              this._markFailed(rec, 'B2', 'NET_ERR', '补手机号异常: ' + err.message);
              this.log('error', 'B2', rec.waybillNo + ' -> 补手机号异常：' + err.message);
              continue;
            }
            if (this.config.delayMs > 0) await sleep(this.config.delayMs);
          }
        }

        // 模板编码与内容
        var tplCode = '';
        try {
          tplCode = await this._resolveTemplateCode(rec.bizProductNo);
          if (!tplCode) throw new Error('未能获取有效打印模板编码');
          var tplContent = await this._getModelContent(tplCode);
          if (!tplContent) throw new Error('模板内容为空(' + tplCode + ')');
          rec.templateCode = tplCode;
          if (!byTemplate[tplCode]) byTemplate[tplCode] = [];
          byTemplate[tplCode].push(rec);
        } catch (err) {
          if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
          this._markFailed(rec, 'B3', 'MODEL_ERR', err.message);
          this.log('error', 'B3', rec.waybillNo + ' -> 获取模板失败：' + err.message);
        }
      }

      // B4. 按模板分组调用 print3（只触发后台打印状态标记，不调用 LODOP.PRINT）
      var tplCodes = Object.keys(byTemplate);
      for (var t = 0; t < tplCodes.length; t++) {
        var code = tplCodes[t];
        var grp = byTemplate[code];
        await this.checkpoint();
        var grpNos = grp.map(function (r) { return r.waybillNo; }).join(',');

        if (this.config.dryRun) {
          var dryTime = formatTimeHMS(this.nowFn());
          for (var g0 = 0; g0 < grp.length; g0++) {
            grp[g0].status = STATES.B_PRINTED;
            grp[g0].stage = 'B4';
            grp[g0].bTime = dryTime;
          }
          this.log(
            'info',
            'B4',
            '[演练] 将调用 print3 标记打印 ' + grp.length + ' 单（模板 ' + code + '）'
          );
          continue;
        }

        try {
          var pResp = await this.post(
            'waybillprint/print3',
            {
              printModel: code,
              model: this.templateContentCache[code] || '',
              waybillNo: grpNos,
              bizProductNo1: '',
              updateFlag: '1',
              printType: '0',
              printradio1: '0',
              printradio2: '0',
              printradio3: '0',
              printradio4: '0',
              insertSensitive: '1',
            },
            { retries: 0, stage: 'B4' }
          );
          if (!pResp || pResp.resCode !== 'A00001') {
            var pErr = (pResp && (pResp.resMess || pResp.resMsg)) || 'print3 返回非 A00001';
            for (var g1 = 0; g1 < grp.length; g1++) {
              this._markFailed(grp[g1], 'B4', (pResp && pResp.resCode) || 'PRINT_ERR', pErr);
            }
            this.log('error', 'B4', 'print3 失败（' + grp.length + ' 单）：' + pErr);
          } else {
            var overMap = Object.create(null);
            if (Array.isArray(pResp.comment)) {
              for (var c0 = 0; c0 < pResp.comment.length; c0++) {
                overMap[pResp.comment[c0]] = true;
              }
            }
            var printedMap = Object.create(null);
            var pDetail = Array.isArray(pResp.detail) ? pResp.detail : [];
            for (var d0 = 0; d0 < pDetail.length; d0++) {
              if (pDetail[d0] && pDetail[d0].waybillNo) {
                printedMap[pDetail[d0].waybillNo] = true;
              }
            }
            var pTime = formatTimeHMS(this.nowFn());
            var okCnt = 0;
            for (var g2 = 0; g2 < grp.length; g2++) {
              var r2 = grp[g2];
              var inDetail = pDetail.length === 0 || !!printedMap[r2.waybillNo];
              if (overMap[r2.waybillNo] && !printedMap[r2.waybillNo]) {
                this._markFailed(r2, 'B4', 'MAX_PRINT', '超过最大打印次数');
              } else if (!inDetail) {
                this._markFailed(r2, 'B4', 'NOT_PRINTED', 'print3 响应未包含该单');
              } else {
                if (overMap[r2.waybillNo]) {
                  appendNotice(r2, '提示超过最大打印次数');
                }
                r2.status = STATES.B_PRINTED;
                r2.stage = 'B4';
                r2.bTime = pTime;
                okCnt++;
              }
            }
            this.log('ok', 'B4', 'print3 成功标记 ' + okCnt + '/' + grp.length + ' 单（模板 ' + code + '）');
          }
        } catch (err) {
          if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
          for (var g3 = 0; g3 < grp.length; g3++) {
            this._markFailed(grp[g3], 'B4', 'NET_ERR', 'print3 异常: ' + err.message);
          }
          this.log('error', 'B4', 'print3 异常：' + err.message);
        }
        if (this.config.delayMs > 0) await sleep(this.config.delayMs);
      }

      processed += batch.length;
      this.emitProgress('B补打', processed, targets.length);
    }
  };

  ReturnAutoEngine.prototype._filterIncompleteOneBills = function () {
    if (!this.config.skipIncompleteOneBill) return;
    var groups = Object.create(null);
    for (var i = 0; i < this.records.length; i++) {
      var rec = this.records[i];
      if (rec.status !== STATES.READY_A2 || !rec.roadsegObj) continue;
      var obj = rec.roadsegObj;
      var flag = String(obj.oneBillFlag || '');
      var total = parseInt(obj.oneBillTotalNum, 10) || 0;
      if (flag && flag !== '0' && total > 1) {
        var mainNo = String(obj.mainWaybillNo || rec.waybillNo);
        if (!groups[mainNo]) groups[mainNo] = { total: total, items: [] };
        groups[mainNo].items.push(rec);
      }
    }
    var mainNos = Object.keys(groups);
    for (var m = 0; m < mainNos.length; m++) {
      var g = groups[mainNos[m]];
      if (g.items.length < g.total) {
        for (var j = 0; j < g.items.length; j++) {
          this._markSkipped(
            g.items[j],
            'A2',
            'ONE_BILL_INCOMPLETE',
            '一票多件未凑齐(' + g.items.length + '/' + g.total + ')，按安全策略跳过'
          );
          this.log(
            'warn',
            'A2',
            g.items[j].waybillNo + ' -> 一票多件未凑齐（主单 ' + mainNos[m] + '），跳过'
          );
        }
      }
    }
  };

  ReturnAutoEngine.prototype._runStageA2 = async function (targets) {
    if (!targets.length) return;
    this.log(
      'info',
      'A2下段',
      '开始批量确认下段 ' + targets.length + ' 单（道段 ' + this.config.roadseg + '，每批 ' + this.config.batchSizeA + ' 单）'
    );
    var batches = chunkArray(targets, this.config.batchSizeA);
    var processed = 0;

    for (var b = 0; b < batches.length; b++) {
      var batch = batches[b];
      await this.checkpoint();

      if (this.config.dryRun) {
        var dryTime = formatTimeHMS(this.nowFn());
        for (var i0 = 0; i0 < batch.length; i0++) {
          batch[i0].status = STATES.A2_DONE;
          batch[i0].stage = 'A2';
          batch[i0].a2Time = dryTime;
        }
        this.log(
          'info',
          'A2下段',
          '[演练] 批次 ' + (b + 1) + '/' + batches.length + ' 将提交 setbilllist 确认下段 ' + batch.length + ' 单'
        );
        processed += batch.length;
        this.emitProgress('A2下段', processed, targets.length);
        continue;
      }

      var maillist = batch.map(function (r) { return r.roadsegObj; });
      var payload = {
        namelist: '',
        maillist: JSON.stringify(maillist),
        type: '2',
        roadseg: this.config.roadseg,
        roadsegname: this.config.roadsegName || this.config.roadseg,
        dlvpersonno: '',
        dlvpersonname: '',
        societyFlag: '',
        tongqian: '',
        carLineValue: '',
        carLineName: '',
        exchangeName: '',
        selfTakeCode: '',
        isItProvided: '',
        orgAttribute: '',
        isConfirm: '0',
      };

      try {
        var resp = await this.post('setroadseg/setbilllist', payload, {
          retries: 0,
          stage: 'A2',
        });
        if (resp && resp.resCode === 'A00001') {
          var okTime = formatTimeHMS(this.nowFn());
          for (var i1 = 0; i1 < batch.length; i1++) {
            batch[i1].status = STATES.A2_DONE;
            batch[i1].stage = 'A2';
            batch[i1].a2Time = okTime;
          }
          this.log(
            'ok',
            'A2下段',
            '批次 ' + (b + 1) + '/' + batches.length + ' 确认下段成功（' + batch.length + ' 单）'
          );
        } else {
          var errMsg = (resp && (resp.resMsg || resp.resMess)) || 'setbilllist 返回非 A00001';
          for (var i2 = 0; i2 < batch.length; i2++) {
            this._markFailed(batch[i2], 'A2', (resp && resp.resCode) || 'SET_ERR', errMsg);
          }
          this.log('error', 'A2下段', '批次 ' + (b + 1) + ' 确认下段失败：' + errMsg);
        }
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        for (var i3 = 0; i3 < batch.length; i3++) {
          this._markFailed(batch[i3], 'A2', 'NET_ERR', 'setbilllist 异常: ' + err.message);
        }
        this.log('error', 'A2下段', '批次 ' + (b + 1) + ' 确认下段异常：' + err.message);
      }

      processed += batch.length;
      this.emitProgress('A2下段', processed, targets.length);
      if (this.config.delayMs > 0 && b < batches.length - 1) {
        await sleep(this.config.delayMs);
      }
    }
  };

  ReturnAutoEngine.prototype._runStageC = async function (targets, upstreamSimulated) {
    if (!targets.length) return;
    this.log('info', 'C妥投', '开始退回妥投处理 ' + targets.length + ' 单（签收类型 ' + this.config.signType + '）');

    // C1. 退回查单
    var c1Passed = [];
    if (this.config.dryRun && upstreamSimulated) {
      for (var s0 = 0; s0 < targets.length; s0++) {
        targets[s0].status = STATES.C1_OK;
        targets[s0].stage = 'C1';
        c1Passed.push(targets[s0]);
      }
      this.log(
        'info',
        'C1',
        '[演练] 上游 A2 未真实落库，跳过真实 C1 查单，直接模拟 ' + targets.length + ' 单进入 C2 预览'
      );
    } else {
      for (var i = 0; i < targets.length; i++) {
        var rec = targets[i];
        await this.checkpoint();
        try {
          var qResp = await this.post(
            'signwaybillreturned/querydeliverinfomanager',
            { waybillNo: rec.waybillNo },
            { retries: 2, stage: 'C1' }
          );
          if (!qResp || qResp.resCode !== 'A00001') {
            var c1Err = (qResp && (qResp.resMess || qResp.resMsg)) || '退回查单失败';
            this._markFailed(rec, 'C1', (qResp && qResp.resCode) || 'B00001', c1Err);
            this.log('error', 'C1', rec.waybillNo + ' -> 查单失败：' + c1Err);
          } else {
            var cm = qResp.comment || {};
            if (
              cm.oneBillFlag === undefined &&
              cm.returnworkState !== undefined &&
              cm.returnworkState !== '' &&
              String(cm.returnworkState) !== '0'
            ) {
              this._markFailed(
                rec,
                'C1',
                'RETURNWORK_STATE',
                '邮件归班状态为 ' + cm.returnworkState + '（非 0），前端规则不允许退回妥投'
              );
              this.log('warn', 'C1', rec.waybillNo + ' -> 归班状态非 0，跳过妥投');
            } else {
              rec.status = STATES.C1_OK;
              rec.stage = 'C1';
              c1Passed.push(rec);
            }
          }
        } catch (err) {
          if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
          this._markFailed(rec, 'C1', 'NET_ERR', 'C1 查单异常: ' + err.message);
          this.log('error', 'C1', rec.waybillNo + ' -> 查单异常：' + err.message);
        }
        this.emitProgress('C1查单', i + 1, targets.length);
        if (this.config.delayMs > 0 && i < targets.length - 1) {
          await sleep(this.config.delayMs);
        }
      }
    }

    if (!c1Passed.length) return;

    // C2. 批量退回妥投确认
    var batches = chunkArray(c1Passed, this.config.batchSizeC);
    var processed = 0;
    for (var b = 0; b < batches.length; b++) {
      var batch = batches[b];
      await this.checkpoint();
      var nos = batch.map(function (r) { return r.waybillNo; }).join(',');

      if (this.config.dryRun) {
        var dryTime = formatTimeHMS(this.nowFn());
        for (var j0 = 0; j0 < batch.length; j0++) {
          batch[j0].status = STATES.C2_DONE;
          batch[j0].stage = 'C2';
          batch[j0].cTime = dryTime;
        }
        this.log(
          'info',
          'C2妥投',
          '[演练] 批次 ' + (b + 1) + '/' + batches.length + ' 将提交 completeconfirmBatch（' + batch.length + ' 单，signType=' + this.config.signType + '）'
        );
        processed += batch.length;
        this.emitProgress('C2妥投', processed, c1Passed.length);
        continue;
      }

      var payload = {
        waybillNo: nos,
        signPersonName: this.config.signPersonName,
        signType: this.config.signType,
        signPersonId: '',
        signPersonIdType: '1',
        agentSignRelation: '',
        netWorkBrand: '',
        netWorkCode: '',
        netWorkName: '',
        netWorkBrandflag: '',
        netWorkBrandCode: '',
        isNophone: '',
        signTwoPicture: '',
        signThreePicture: '',
        signPicture: '',
      };

      try {
        var cResp = await this.post('signwaybillreturned/completeconfirmBatch', payload, {
          retries: 0,
          stage: 'C2',
        });
        if (cResp && cResp.resCode === 'A00001') {
          var okTime = formatTimeHMS(this.nowFn());
          for (var j1 = 0; j1 < batch.length; j1++) {
            batch[j1].status = STATES.C2_DONE;
            batch[j1].stage = 'C2';
            batch[j1].cTime = okTime;
          }
          this.log(
            'ok',
            'C2妥投',
            '批次 ' + (b + 1) + '/' + batches.length + ' 退回妥投成功（' + batch.length + ' 单）'
          );
        } else {
          var c2Err = (cResp && (cResp.resMess || cResp.resMsg)) || 'completeconfirmBatch 失败';
          for (var j2 = 0; j2 < batch.length; j2++) {
            this._markFailed(batch[j2], 'C2', (cResp && cResp.resCode) || 'B00001', c2Err);
          }
          this.log('error', 'C2妥投', '批次 ' + (b + 1) + ' 妥投失败：' + c2Err);
        }
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        for (var j3 = 0; j3 < batch.length; j3++) {
          this._markFailed(batch[j3], 'C2', 'NET_ERR', 'C2 妥投异常: ' + err.message);
        }
        this.log('error', 'C2妥投', '批次 ' + (b + 1) + ' 妥投异常：' + err.message);
      }

      processed += batch.length;
      this.emitProgress('C2妥投', processed, c1Passed.length);
      if (this.config.delayMs > 0 && b < batches.length - 1) {
        await sleep(this.config.delayMs);
      }
    }
  };

  ReturnAutoEngine.prototype._queryReturnWorkTasks = async function () {
    var today = formatDateYMD(this.nowFn());
    var dlvPerson = this.config.dlvPersonName;
    var segCode = this.config.returnWorkRoadSegCode || (dlvPerson ? '' : this.config.roadseg);
    return this.post(
      'returnwork/querydelivertask',
      {
        receiptFlag: '',
        dlvPersonName: dlvPerson,
        beginTime: today + ' 00:00:00',
        endTime: today + ' 23:59:59',
        dlvState: '',
        roadSegCode: segCode,
        postType: '',
        baseProductNo: '',
      },
      { retries: 2, stage: 'D1' }
    );
  };

  ReturnAutoEngine.prototype._runStageD = async function (targets, upstreamSimulated) {
    if (!targets.length) return;
    var today = formatDateYMD(this.nowFn());
    var dlvPerson = this.config.dlvPersonName;
    var segCode = this.config.returnWorkRoadSegCode || (dlvPerson ? '' : this.config.roadseg);

    this.log(
      'info',
      'D归班',
      '开始归班交接 ' +
        targets.length +
        ' 单（日期 ' +
        today +
        '，投递员=' +
        (dlvPerson || '未填') +
        '，道段=' +
        (segCode || '未限') +
        '）'
    );

    if (this.config.dryRun && upstreamSimulated) {
      var simTime = formatTimeHMS(this.nowFn());
      for (var s0 = 0; s0 < targets.length; s0++) {
        targets[s0].status = STATES.DONE;
        targets[s0].stage = 'D2';
        targets[s0].dTime = simTime;
      }
      this.log(
        'info',
        'D归班',
        '[演练] 上游未真实落库，将按本次成功清单严格交集后提交 acceptdelivertask（' + targets.length + ' 单）'
      );
      this.emitProgress('D归班', targets.length, targets.length);
      return;
    }

    // D1. 查询今日归班任务（与本次目标单号严格求交集，绝不误交接同投递员的其他邮件）
    var targetSet = Object.create(null);
    for (var i = 0; i < targets.length; i++) targetSet[targets[i].waybillNo] = targets[i];

    var matchedMap = Object.create(null);
    for (var round = 0; round < 2; round++) {
      await this.checkpoint();
      var qResp;
      try {
        qResp = await this._queryReturnWorkTasks();
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        for (var e0 = 0; e0 < targets.length; e0++) {
          this._markFailed(targets[e0], 'D1', 'NET_ERR', '归班任务查询异常: ' + err.message);
        }
        this.log('error', 'D1', '归班任务查询异常：' + err.message);
        return;
      }

      var list = qResp && qResp.resCode !== 'B00001' && Array.isArray(qResp.detail) ? qResp.detail : [];
      for (var k = 0; k < list.length; k++) {
        var wb = list[k] && list[k].waybillNo;
        if (wb && targetSet[wb]) {
          matchedMap[wb] = true;
        }
      }

      var matchedCount = Object.keys(matchedMap).length;
      if (matchedCount >= targets.length || round === 1) {
        this.log(
          'info',
          'D1',
          '归班列表共返回 ' +
            list.length +
            ' 单，严格匹配本次目标 ' +
            matchedCount +
            '/' +
            targets.length +
            ' 单'
        );
        break;
      }
      this.log(
        'warn',
        'D1',
        '尚有 ' + (targets.length - matchedCount) + ' 单未出现在归班列表，等待 1.2 秒后重查一次...'
      );
      await sleep(1200);
    }

    var matchedTargets = [];
    for (var m = 0; m < targets.length; m++) {
      var r = targets[m];
      if (matchedMap[r.waybillNo]) {
        matchedTargets.push(r);
      } else {
        this._markFailed(
          r,
          'D1',
          'D1_NOT_FOUND',
          '归班任务列表中未查到该单（请核对投递员设置或稍后仅勾选D阶段补跑）'
        );
        this.log('warn', 'D1', r.waybillNo + ' -> 未出现在今日归班列表');
      }
    }

    if (!matchedTargets.length) return;

    // D2. 确认归班交接
    var batches = chunkArray(matchedTargets, this.config.batchSizeD);
    var processed = 0;
    for (var b = 0; b < batches.length; b++) {
      var batch = batches[b];
      await this.checkpoint();
      var nos = batch.map(function (r) { return r.waybillNo; }).join(',');

      if (this.config.dryRun) {
        var dryTime = formatTimeHMS(this.nowFn());
        for (var j0 = 0; j0 < batch.length; j0++) {
          batch[j0].status = STATES.DONE;
          batch[j0].stage = 'D2';
          batch[j0].dTime = dryTime;
        }
        this.log(
          'info',
          'D2交接',
          '[演练] 批次 ' + (b + 1) + '/' + batches.length + ' 将提交 acceptdelivertask 归班交接 ' + batch.length + ' 单'
        );
        processed += batch.length;
        this.emitProgress('D2交接', processed, matchedTargets.length);
        continue;
      }

      try {
        var dResp = await this.post(
          'returnwork/acceptdelivertask',
          { waybillNo: nos, type: '' },
          { retries: 0, stage: 'D2' }
        );
        if (!dResp || dResp.resCode === 'B00001') {
          var dErr = (dResp && (dResp.resMess || dResp.resMsg)) || 'acceptdelivertask 失败';
          for (var j1 = 0; j1 < batch.length; j1++) {
            this._markFailed(batch[j1], 'D2', (dResp && dResp.resCode) || 'B00001', dErr);
          }
          this.log('error', 'D2交接', '批次 ' + (b + 1) + ' 归班交接失败：' + dErr);
        } else {
          var okTime = formatTimeHMS(this.nowFn());
          var cm = dResp.comment && typeof dResp.comment === 'object' ? dResp.comment : null;
          var okCnt = 0;
          for (var j2 = 0; j2 < batch.length; j2++) {
            var rec2 = batch[j2];
            if (cm && Object.prototype.hasOwnProperty.call(cm, rec2.waybillNo) && String(cm[rec2.waybillNo]) === '0') {
              this._markFailed(rec2, 'D2', 'ACCEPT_ZERO', '服务端返回该单交接结果为 0');
            } else {
              rec2.status = STATES.DONE;
              rec2.stage = 'D2';
              rec2.dTime = okTime;
              okCnt++;
            }
          }
          this.log(
            'ok',
            'D2交接',
            '批次 ' + (b + 1) + '/' + batches.length + ' 归班交接完成（' + okCnt + '/' + batch.length + ' 单）'
          );
        }
      } catch (err) {
        if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
        for (var j3 = 0; j3 < batch.length; j3++) {
          this._markFailed(batch[j3], 'D2', 'NET_ERR', 'D2 归班异常: ' + err.message);
        }
        this.log('error', 'D2交接', '批次 ' + (b + 1) + ' 归班异常：' + err.message);
      }

      processed += batch.length;
      this.emitProgress('D2交接', processed, matchedTargets.length);
      if (this.config.delayMs > 0 && b < batches.length - 1) {
        await sleep(this.config.delayMs);
      }
    }
  };

  ReturnAutoEngine.prototype._promoteFinalSuccess = function () {
    var rs = this.config.runStages;
    var terminalSuccessStatus = null;
    if (rs.D) terminalSuccessStatus = STATES.DONE;
    else if (rs.C) terminalSuccessStatus = STATES.C2_DONE;
    else if (rs.A) terminalSuccessStatus = STATES.A2_DONE;
    else if (rs.B) terminalSuccessStatus = STATES.B_PRINTED;

    if (!terminalSuccessStatus || terminalSuccessStatus === STATES.DONE) return;
    for (var i = 0; i < this.records.length; i++) {
      if (this.records[i].status === terminalSuccessStatus) {
        this.records[i].status = STATES.DONE;
      }
    }
  };

  ReturnAutoEngine.prototype.run = async function (waybills) {
    var errs = this.validateConfig(waybills);
    if (errs.length > 0) {
      throw new Error(errs.join('；'));
    }

    this._initRecords(waybills);
    this.running = true;
    this.paused = false;
    this.stopped = false;
    this.emitState('running');

    var rs = this.config.runStages;
    var stageDesc = ['A', 'B', 'C', 'D'].filter(function (k) { return !!rs[k]; }).join('→');
    this.log(
      'info',
      'SYS',
      '启动任务：共 ' +
        waybills.length +
        ' 单 | 阶段 ' +
        stageDesc +
        ' | 模式：' +
        (this.config.dryRun ? '演练模式（不执行写入提交）' : '正式执行模式')
    );

    var finalState = 'completed';
    try {
      // 自动识别当前登录账号绑定的道段与投递员（当未显式指定时）
      var needAutoRoadseg = rs.A && (!this.config.roadseg || this.config.roadseg.toUpperCase() === 'AUTO');
      var needAutoCourier = rs.D && !this.config.dlvPersonName;
      if (needAutoRoadseg || needAutoCourier) {
        try {
          var detected = await this.detectCurrentAccount();
          if (needAutoRoadseg && detected.roadseg) {
            this.config.roadseg = detected.roadseg;
            this.config.roadsegName = detected.roadsegName || detected.roadseg;
          }
          if (needAutoCourier && detected.dlvPersonName) {
            this.config.dlvPersonName = detected.dlvPersonName;
          }
          if (detected.roadseg || detected.dlvPersonName) {
            this.log(
              'info',
              'INIT',
              '已自动识别账号绑定参数：道段=' +
                (this.config.roadseg || '未识别') +
                '，投递员=' +
                (this.config.dlvPersonName || '未识别')
            );
          }
        } catch (err) {
          if (err && (err.name === 'SessionExpiredError' || err.isStop)) throw err;
          this.log('warn', 'INIT', '自动识别账号绑定信息失败：' + err.message);
        }
      }

      if (rs.A && (!this.config.roadseg || this.config.roadseg.toUpperCase() === 'AUTO')) {
        throw new Error('未能自动识别当前账号绑定的道段，请在高级参数中手动填写或先打开一次“下段扫描”页');
      }
      if (rs.D && !this.config.dlvPersonName && !this.config.returnWorkRoadSegCode && !this.config.roadseg) {
        throw new Error('未能自动识别归班投递员，请点击“重新识别账号”或在输入框中选择投递员');
      }

      // 阶段 A1 + B + A1重扫 + A2
      if (rs.A) {
        await this._runStageA1(this.records, false);

        var needReprint = this.records.filter(function (r) {
          return r.status === STATES.NEED_REPRINT;
        });
        if (needReprint.length > 0) {
          if (rs.B) {
            await this._runStageB(needReprint);
            var printed = needReprint.filter(function (r) {
              return r.status === STATES.B_PRINTED;
            });
            if (printed.length > 0) {
              if (this.config.dryRun) {
                for (var p0 = 0; p0 < printed.length; p0++) {
                  printed[p0].status = STATES.READY_A2;
                  printed[p0].stage = 'A1_RETRY';
                }
                this.log(
                  'info',
                  'A1重扫',
                  '[演练] 补打标记后将对 ' + printed.length + ' 单回流重扫 queryiteminfo 并进入 A2 下段'
                );
              } else {
                await this._runStageA1(printed, true);
              }
            }
          } else {
            for (var n0 = 0; n0 < needReprint.length; n0++) {
              this._markSkipped(
                needReprint[n0],
                'B',
                'B_DISABLED',
                '撤单邮件需补打，但当前未勾选 B 阶段'
              );
            }
          }
        }

        this._filterIncompleteOneBills();
        var readyA2 = this.records.filter(function (r) {
          return r.status === STATES.READY_A2;
        });
        await this._runStageA2(readyA2);
      } else if (rs.B) {
        await this._runStageB(this.records);
      }

      // 阶段 C（退回妥投）
      if (rs.C) {
        var cTargets = this.records.filter(function (r) {
          if (rs.A) return r.status === STATES.A2_DONE;
          if (rs.B) return r.status === STATES.B_PRINTED;
          return r.status === STATES.PENDING;
        });
        await this._runStageC(cTargets, this.config.dryRun && rs.A);
      }

      // 阶段 D（归班交接）
      if (rs.D) {
        var dTargets = this.records.filter(function (r) {
          if (rs.C) return r.status === STATES.C2_DONE;
          if (rs.A) return r.status === STATES.A2_DONE;
          if (rs.B) return r.status === STATES.B_PRINTED;
          return r.status === STATES.PENDING;
        });
        await this._runStageD(dTargets, this.config.dryRun && (rs.C || rs.A));
      }

      this._promoteFinalSuccess();
    } catch (err) {
      if (err && err.isStop) {
        finalState = 'stopped';
        this.log('warn', 'SYS', '任务已由用户中止');
      } else if (err && err.name === 'SessionExpiredError') {
        finalState = 'error';
        this.log('error', 'AUTH', err.message);
      } else {
        finalState = 'error';
        this.log('error', 'SYS', '任务异常中断：' + (err ? err.message : String(err)));
      }
    } finally {
      this.running = false;
      this.paused = false;
      var summary = this.getSummary();
      this.log(
        finalState === 'completed' ? 'ok' : 'warn',
        'SYS',
        '执行结束（' +
          finalState +
          '）：总计 ' +
          summary.total +
          ' | 完成 ' +
          summary.done +
          ' | 补打 ' +
          summary.reprinted +
          ' | 跳过 ' +
          summary.skipped +
          ' | 失败 ' +
          summary.failed
      );
      this.emitProgress('已完成', summary.total, summary.total);
      this.emitState(finalState);
    }

    return {
      state: finalState,
      summary: this.getSummary(),
      records: this.records.slice(),
    };
  };

  // =========================================================================
  // 4. 浏览器悬浮控制面板 UI（零依赖，自动适配内网 Edge 页面）
  // =========================================================================

  function loadSavedConfig() {
    var base = Object.assign({}, DEFAULT_CONFIG, { ctx: detectCtx() });
    try {
      if (typeof localStorage !== 'undefined') {
        var raw = localStorage.getItem(STORAGE_KEY);
        if (raw) {
          var saved = JSON.parse(raw);
          if (saved && typeof saved === 'object') {
            base = Object.assign(base, saved);
            base.runStages = Object.assign({ A: true, B: true, C: true, D: true }, saved.runStages || {});
          }
        }
      }
    } catch (e) {}
    return base;
  }

  function saveConfig(cfg) {
    try {
      if (typeof localStorage !== 'undefined') {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
      }
    } catch (e) {}
  }

  function triggerDownload(filename, content) {
    var blob = new Blob([content], { type: 'text/csv;charset=utf-8;' });
    var url = URL.createObjectURL(blob);
    var a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    setTimeout(function () {
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    }, 300);
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } catch (e) {}
    document.body.removeChild(ta);
    return Promise.resolve();
  }

  function isIntranetContext(loc) {
    var l = loc || (typeof window !== 'undefined' ? window.location : null);
    if (!l) return false;
    var p = String(l.pathname || '');
    if (p.indexOf('/cas/login') === 0) return false;
    return p.indexOf('/delivery-web') === 0 || p.indexOf('/portal') === 0;
  }

  function togglePanel() {
    if (typeof document === 'undefined') return;
    var rootEl = document.getElementById('ra-panel-root');
    if (!rootEl) {
      mountPanel({ startHidden: false });
      return;
    }
    var fab = document.getElementById('ra-fab-btn');
    var hidden = rootEl.style.display === 'none';
    rootEl.style.display = hidden ? 'block' : 'none';
    if (fab) fab.style.display = hidden ? 'none' : 'inline-flex';
    if (hidden) {
      var bodyEl = rootEl.querySelector('#ra-body');
      if (bodyEl) bodyEl.style.display = 'block';
      var toggleBtn = rootEl.querySelector('#ra-btn-toggle');
      if (toggleBtn) toggleBtn.textContent = '收起';
      if (typeof rootEl.__raOnOpen === 'function') rootEl.__raOnOpen();
    }
  }

  function mountPanel(mountOpts) {
    if (typeof document === 'undefined') return null;
    var opts = mountOpts || {};
    var existing = document.getElementById('ra-panel-root');
    if (existing) {
      if (!opts.startHidden) {
        existing.style.display = 'block';
        var bodyEl = existing.querySelector('#ra-body');
        if (bodyEl) bodyEl.style.display = 'block';
        var fabEl = document.getElementById('ra-fab-btn');
        if (fabEl) fabEl.style.display = 'none';
        if (typeof existing.__raOnOpen === 'function') existing.__raOnOpen();
      }
      return existing;
    }

    var styleId = 'ra-panel-style';
    if (!document.getElementById(styleId)) {
      var style = document.createElement('style');
      style.id = styleId;
      style.textContent = [
        '#ra-panel-root{position:fixed;top:18px;right:18px;width:540px;max-width:95vw;background:#fff;color:#1f2937;border:1px solid #cbd5e1;border-radius:10px;box-shadow:0 12px 32px rgba(15,23,42,.22);font:12px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI","Microsoft YaHei",sans-serif;z-index:2147483640;overflow:hidden}',
        '#ra-header{display:flex;align-items:center;justify-content:space-between;padding:8px 12px;background:#0f172a;color:#f8fafc;cursor:move;user-select:none}',
        '#ra-header .ra-title{font-weight:700;font-size:13px;display:flex;align-items:center;gap:8px}',
        '.ra-badge{padding:2px 7px;border-radius:999px;font-size:11px;font-weight:600}',
        '.ra-badge-dry{background:#f59e0b;color:#111827}.ra-badge-real{background:#ef4444;color:#fff}',
        '#ra-header button{background:transparent;border:1px solid rgba(255,255,255,.25);color:#f8fafc;border-radius:4px;padding:2px 8px;cursor:pointer;font-size:11px;margin-left:4px}',
        '#ra-body{padding:10px 12px;max-height:82vh;overflow-y:auto}',
        '.ra-warn-bar{background:#fef3c7;border:1px solid #f59e0b;color:#92400e;padding:6px 8px;border-radius:6px;margin-bottom:8px;font-size:11px}',
        '.ra-grid{display:grid;grid-template-columns:1fr 1fr;gap:6px 8px;margin-bottom:8px}',
        '.ra-field{display:flex;flex-direction:column;gap:2px}.ra-field label{font-size:11px;color:#475569;font-weight:600}',
        '.ra-field input,.ra-field select{height:26px;padding:2px 6px;border:1px solid #cbd5e1;border-radius:5px;font-size:12px;box-sizing:border-box;width:100%}',
        '.ra-row{display:flex;flex-wrap:wrap;align-items:center;gap:8px;margin-bottom:8px}',
        '.ra-row label{display:inline-flex;align-items:center;gap:3px;cursor:pointer;font-size:12px}',
        '.ra-btn{border:1px solid #cbd5e1;background:#f8fafc;color:#1e293b;border-radius:5px;padding:4px 9px;cursor:pointer;font-size:12px;font-weight:600}',
        '.ra-btn:hover:not(:disabled){background:#e2e8f0}.ra-btn:disabled{opacity:.45;cursor:not-allowed}',
        '.ra-btn-primary{background:#2563eb;border-color:#2563eb;color:#fff}.ra-btn-primary:hover:not(:disabled){background:#1d4ed8}',
        '.ra-btn-danger{background:#dc2626;border-color:#dc2626;color:#fff}.ra-btn-danger:hover:not(:disabled){background:#b91c1c}',
        '.ra-btn-warn{background:#d97706;border-color:#d97706;color:#fff}',
        '#ra-input{width:100%;height:96px;box-sizing:border-box;border:1px dashed #94a3b8;border-radius:6px;padding:6px 8px;font-family:Consolas,monospace;font-size:12px;resize:vertical}',
        '#ra-input.ra-drag{border-color:#2563eb;background:#eff6ff}',
        '.ra-kpis{display:grid;grid-template-columns:repeat(5,1fr);gap:6px;margin:8px 0}',
        '.ra-kpi{background:#f8fafc;border:1px solid #e2e8f0;border-radius:6px;padding:4px 6px;text-align:center}',
        '.ra-kpi b{display:block;font-size:14px}.ra-kpi span{font-size:11px;color:#64748b}',
        '.ra-prog-wrap{height:8px;background:#e2e8f0;border-radius:999px;overflow:hidden;margin-bottom:4px}',
        '.ra-prog-bar{height:100%;width:0;background:#2563eb;transition:width .2s}',
        '.ra-tabs{display:flex;gap:6px;margin:6px 0 4px;border-bottom:1px solid #e2e8f0;padding-bottom:4px;align-items:center}',
        '.ra-tab{padding:3px 8px;border-radius:5px;cursor:pointer;font-weight:600;color:#475569}.ra-tab.active{background:#dbeafe;color:#1d4ed8}',
        '#ra-log-box{height:165px;overflow-y:auto;background:#0f172a;color:#e2e8f0;border-radius:6px;padding:6px 8px;font:11px/1.5 Consolas,monospace;white-space:pre-wrap;word-break:break-all}',
        '.ra-log-ok{color:#4ade80}.ra-log-warn{color:#fbbf24}.ra-log-error{color:#f87171}.ra-log-info{color:#93c5fd}',
        '#ra-table-wrap{height:165px;overflow:auto;border:1px solid #e2e8f0;border-radius:6px;display:none}',
        '#ra-table{width:100%;border-collapse:collapse;font-size:11px}#ra-table th,#ra-table td{border-bottom:1px solid #e2e8f0;padding:3px 5px;text-align:left;white-space:nowrap}',
        '#ra-table th{background:#f1f5f9;position:sticky;top:0}',
        'details.ra-adv{border:1px solid #e2e8f0;border-radius:6px;padding:5px 8px;margin-bottom:8px;background:#f8fafc}',
        'details.ra-adv summary{cursor:pointer;font-weight:600;color:#334155;font-size:11px}',
        '.ra-acct-bar{display:flex;align-items:center;justify-content:space-between;gap:8px;background:#f0f9ff;border:1px solid #bae6fd;color:#0c4a6e;padding:5px 8px;border-radius:6px;margin-bottom:8px;font-size:11px}',
        '#ra-fab-btn{position:fixed;bottom:24px;right:24px;z-index:2147483640;display:none;align-items:center;gap:6px;padding:8px 14px;border-radius:999px;border:1px solid #1d4ed8;background:#2563eb;color:#fff;font:600 13px/1.2 -apple-system,BlinkMacSystemFont,"Segoe UI","Microsoft YaHei",sans-serif;box-shadow:0 8px 22px rgba(37,99,235,.35);cursor:pointer;user-select:none}',
        '#ra-fab-btn:hover{background:#1d4ed8}',
      ].join('');
      document.head.appendChild(style);
    }

    var cfg = loadSavedConfig();
    var rootEl = document.createElement('div');
    rootEl.id = 'ra-panel-root';

    var onIntranetApp = isIntranetContext();

    rootEl.innerHTML = [
      '<div id="ra-header">',
      '  <div class="ra-title"><span>退货自动处理助手 v' + VERSION + '</span><span id="ra-mode-badge" class="ra-badge ra-badge-dry">演练模式</span></div>',
      '  <div><button id="ra-btn-toggle" type="button">收起</button><button id="ra-btn-close" type="button">×</button></div>',
      '</div>',
      '<div id="ra-body">',
      onIntranetApp ? '' : '<div class="ra-warn-bar">提示：当前页面不是内网业务页（/portal/ 或 /delivery-web/），请先登录内网投递系统后再执行。</div>',
      '  <div class="ra-acct-bar">',
      '    <span id="ra-acct-text">正在自动识别当前登录账号与系统绑定信息...</span>',
      '    <button id="ra-btn-fetch" class="ra-btn" type="button" style="white-space:nowrap">重新识别账号</button>',
      '  </div>',
      '  <div class="ra-grid">',
      '    <div class="ra-field"><label>归班投递员（自动识别，可下拉改选）</label><input id="ra-courier" list="ra-courier-list" placeholder="自动识别中..."/><datalist id="ra-courier-list"></datalist></div>',
      '    <div class="ra-field"><label>补打固定手机号（11位）</label><input id="ra-mobile" placeholder="13000000000"/></div>',
      '  </div>',
      '  <div class="ra-row">',
      '    <label><input id="ra-st-a" type="checkbox"/> A下段</label>',
      '    <label><input id="ra-st-b" type="checkbox"/> B补打</label>',
      '    <label><input id="ra-st-c" type="checkbox"/> C妥投</label>',
      '    <label><input id="ra-st-d" type="checkbox"/> D归班</label>',
      '    <label style="margin-left:auto;color:#b45309;font-weight:700"><input id="ra-dryrun" type="checkbox"/> 演练模式</label>',
      '  </div>',
      '  <details class="ra-adv"><summary>高级参数与安全选项（含系统自动识别的道段与模板，一般无需修改）</summary>',
      '    <div class="ra-grid" style="margin-top:6px">',
      '      <div class="ra-field"><label>下段道段（留空/AUTO自动识别）</label><input id="ra-roadseg" list="ra-roadseg-list" placeholder="自动识别（如 T01）"/><datalist id="ra-roadseg-list"></datalist></div>',
      '      <div class="ra-field"><label>打印模板编码（AUTO自动查）</label><input id="ra-template" placeholder="SD_GN_KB_MD"/></div>',
      '      <div class="ra-field"><label>每批单数 (A2/B/C2/D2)</label><input id="ra-batch" type="number" min="1" max="200"/></div>',
      '      <div class="ra-field"><label>单次请求间隔 (ms)</label><input id="ra-delay" type="number" min="0" max="5000"/></div>',
      '      <div class="ra-field"><label>签收类型代码</label><input id="ra-signtype" placeholder="130"/></div>',
      '      <div class="ra-field"><label>接口上下文路径 ctx</label><input id="ra-ctx" placeholder="/delivery-web/a"/></div>',
      '    </div>',
      '    <div class="ra-row" style="margin-bottom:2px">',
      '      <label><input id="ra-opt-alreadyset" type="checkbox"/> 已下段直接进C</label>',
      '      <label><input id="ra-opt-cod" type="checkbox"/> 安全跳过代收货款</label>',
      '      <label><input id="ra-opt-onebill" type="checkbox"/> 跳过未凑齐一票多件</label>',
      '      <label><input id="ra-opt-zhuanduan" type="checkbox"/> 允许转段件</label>',
      '    </div>',
      '  </details>',
      '  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:4px">',
      '    <span id="ra-parse-info" style="font-size:11px;color:#475569">可粘贴单号（支持换行/逗号/Excel列）或直接拖入 CSV/TXT 文件</span>',
      '    <div style="display:flex;gap:4px">',
      '      <input id="ra-file" type="file" accept=".csv,.txt" style="display:none"/>',
      '      <button id="ra-btn-import" class="ra-btn" type="button">导入CSV</button>',
      '      <button id="ra-btn-retry-fail" class="ra-btn" type="button" disabled>载入失败单</button>',
      '      <button id="ra-btn-clear" class="ra-btn" type="button">清空</button>',
      '    </div>',
      '  </div>',
      '  <textarea id="ra-input" placeholder="粘贴运单号，例如：\n9800000000001\n9800000000002"></textarea>',
      '  <div class="ra-row" style="margin-top:8px">',
      '    <button id="ra-btn-start" class="ra-btn ra-btn-primary" type="button">开始执行</button>',
      '    <button id="ra-btn-pause" class="ra-btn ra-btn-warn" type="button" disabled>暂停</button>',
      '    <button id="ra-btn-stop" class="ra-btn ra-btn-danger" type="button" disabled>中止</button>',
      '    <button id="ra-btn-csv" class="ra-btn" type="button" disabled>导出CSV报表</button>',
      '    <button id="ra-btn-copy-fail" class="ra-btn" type="button" disabled>复制失败单号</button>',
      '  </div>',
      '  <div class="ra-prog-wrap"><div id="ra-prog-bar" class="ra-prog-bar"></div></div>',
      '  <div id="ra-prog-text" style="font-size:11px;color:#475569">待命</div>',
      '  <div class="ra-kpis">',
      '    <div class="ra-kpi"><b id="ra-k-total">0</b><span>总计</span></div>',
      '    <div class="ra-kpi"><b id="ra-k-done" style="color:#16a34a">0</b><span>完成</span></div>',
      '    <div class="ra-kpi"><b id="ra-k-rep" style="color:#2563eb">0</b><span>补打</span></div>',
      '    <div class="ra-kpi"><b id="ra-k-skip" style="color:#d97706">0</b><span>跳过</span></div>',
      '    <div class="ra-kpi"><b id="ra-k-fail" style="color:#dc2626">0</b><span>失败</span></div>',
      '  </div>',
      '  <div class="ra-tabs">',
      '    <span id="ra-tab-log" class="ra-tab active">实时日志</span>',
      '    <span id="ra-tab-tbl" class="ra-tab">结果明细</span>',
      '    <select id="ra-tbl-filter" style="margin-left:auto;height:22px;font-size:11px;display:none"><option value="ALL">全部</option><option value="FAILED">仅失败</option><option value="SKIPPED">仅跳过</option><option value="DONE">已完成</option></select>',
      '  </div>',
      '  <div id="ra-log-box"></div>',
      '  <div id="ra-table-wrap"><table id="ra-table"><thead><tr><th>运单号</th><th>状态</th><th>阶段</th><th>补打</th><th>补手机</th><th>说明/原因</th></tr></thead><tbody id="ra-tbody"></tbody></table></div>',
      '</div>',
    ].join('');

    document.body.appendChild(rootEl);

    var fabBtn = document.getElementById('ra-fab-btn');
    if (!fabBtn) {
      fabBtn = document.createElement('button');
      fabBtn.id = 'ra-fab-btn';
      fabBtn.type = 'button';
      fabBtn.textContent = '退货助手';
      fabBtn.title = '点击打开内网退货自动处理助手';
      fabBtn.addEventListener('click', function () {
        togglePanel();
      });
      document.body.appendChild(fabBtn);
    }

    function $(id) { return rootEl.querySelector('#' + id); }

    // 填充初始配置
    $('ra-roadseg').value = cfg.roadseg;
    $('ra-courier').value = cfg.dlvPersonName;
    $('ra-mobile').value = cfg.fixedMobile;
    $('ra-template').value = cfg.templateCode;
    $('ra-st-a').checked = !!cfg.runStages.A;
    $('ra-st-b').checked = !!cfg.runStages.B;
    $('ra-st-c').checked = !!cfg.runStages.C;
    $('ra-st-d').checked = !!cfg.runStages.D;
    $('ra-dryrun').checked = cfg.dryRun !== false;
    $('ra-batch').value = cfg.batchSizeA;
    $('ra-delay').value = cfg.delayMs;
    $('ra-signtype').value = cfg.signType;
    $('ra-ctx').value = cfg.ctx;
    $('ra-opt-alreadyset').checked = cfg.continueAlreadySetToC !== false;
    $('ra-opt-cod').checked = cfg.skipCod !== false;
    $('ra-opt-onebill').checked = cfg.skipIncompleteOneBill !== false;
    $('ra-opt-zhuanduan').checked = cfg.allowZhuanduan !== false;

    function updateModeBadge() {
      var badge = $('ra-mode-badge');
      var isDry = $('ra-dryrun').checked;
      badge.textContent = isDry ? '演练模式' : '正式模式';
      badge.className = 'ra-badge ' + (isDry ? 'ra-badge-dry' : 'ra-badge-real');
    }
    updateModeBadge();

    function readConfigFromUI() {
      var batch = Math.min(200, Math.max(1, parseInt($('ra-batch').value, 10) || 50));
      var roadseg = $('ra-roadseg').value.trim();
      var next = {
        ctx: $('ra-ctx').value.trim() || detectCtx(),
        roadseg: roadseg,
        roadsegName: roadseg,
        dlvPersonName: $('ra-courier').value.trim(),
        returnWorkRoadSegCode: '',
        fixedMobile: $('ra-mobile').value.trim(),
        templateCode: $('ra-template').value.trim() || 'SD_GN_KB_MD',
        signType: $('ra-signtype').value.trim() || '130',
        signPersonName: '',
        batchSizeA: batch,
        batchSizeB: Math.min(100, batch),
        batchSizeC: batch,
        batchSizeD: batch,
        delayMs: Math.max(0, parseInt($('ra-delay').value, 10) || 0),
        timeoutMs: 30000,
        dryRun: $('ra-dryrun').checked,
        runStages: {
          A: $('ra-st-a').checked,
          B: $('ra-st-b').checked,
          C: $('ra-st-c').checked,
          D: $('ra-st-d').checked,
        },
        continueAlreadySetToC: $('ra-opt-alreadyset').checked,
        skipCod: $('ra-opt-cod').checked,
        skipIncompleteOneBill: $('ra-opt-onebill').checked,
        allowZhuanduan: $('ra-opt-zhuanduan').checked,
      };
      saveConfig(next);
      return next;
    }

    ['ra-roadseg', 'ra-courier', 'ra-mobile', 'ra-template', 'ra-st-a', 'ra-st-b', 'ra-st-c', 'ra-st-d', 'ra-dryrun', 'ra-batch', 'ra-delay', 'ra-signtype', 'ra-ctx', 'ra-opt-alreadyset', 'ra-opt-cod', 'ra-opt-onebill', 'ra-opt-zhuanduan'].forEach(function (id) {
      $(id).addEventListener('change', function () {
        updateModeBadge();
        readConfigFromUI();
      });
    });

    // 拖动与折叠
    var header = $('ra-header');
    var dragging = false, startX = 0, startY = 0, origLeft = 0, origTop = 0;
    header.addEventListener('mousedown', function (e) {
      if (e.target.tagName === 'BUTTON') return;
      dragging = true;
      var rect = rootEl.getBoundingClientRect();
      origLeft = rect.left;
      origTop = rect.top;
      startX = e.clientX;
      startY = e.clientY;
      rootEl.style.right = 'auto';
      rootEl.style.left = origLeft + 'px';
      rootEl.style.top = origTop + 'px';
    });
    document.addEventListener('mousemove', function (e) {
      if (!dragging) return;
      rootEl.style.left = Math.max(0, origLeft + e.clientX - startX) + 'px';
      rootEl.style.top = Math.max(0, origTop + e.clientY - startY) + 'px';
    });
    document.addEventListener('mouseup', function () { dragging = false; });

    $('ra-btn-toggle').addEventListener('click', function () {
      var b = $('ra-body');
      var hidden = b.style.display === 'none';
      b.style.display = hidden ? 'block' : 'none';
      $('ra-btn-toggle').textContent = hidden ? '收起' : '展开';
    });
    $('ra-btn-close').addEventListener('click', function () {
      if (activeEngine && activeEngine.running) {
        if (!window.confirm('任务仍在运行中，确定中止并隐藏面板？')) return;
        activeEngine.stop();
      }
      rootEl.style.display = 'none';
      if (fabBtn) fabBtn.style.display = 'inline-flex';
    });

    // 日志与结果渲染
    var activeEngine = null;
    var lastRecords = [];

    function appendLog(entry) {
      var box = $('ra-log-box');
      var div = document.createElement('div');
      div.className = 'ra-log-' + (entry.level || 'info');
      div.textContent = '[' + entry.time + '][' + entry.stage + '] ' + entry.message;
      box.appendChild(div);
      if (box.childNodes.length > 600) box.removeChild(box.firstChild);
      box.scrollTop = box.scrollHeight;
    }

    function updateSummaryUI(summary, phaseLabel, current, total) {
      $('ra-k-total').textContent = String(summary.total);
      $('ra-k-done').textContent = String(summary.done);
      $('ra-k-rep').textContent = String(summary.reprinted);
      $('ra-k-skip').textContent = String(summary.skipped);
      $('ra-k-fail').textContent = String(summary.failed);
      var pct = total > 0 ? Math.min(100, Math.round((current / total) * 100)) : 0;
      $('ra-prog-bar').style.width = pct + '%';
      $('ra-prog-text').textContent = (phaseLabel || '执行中') + ' (' + current + '/' + total + ', ' + pct + '%)';
    }

    function renderTable() {
      var filter = $('ra-tbl-filter').value;
      var tbody = $('ra-tbody');
      tbody.innerHTML = '';
      for (var i = 0; i < lastRecords.length; i++) {
        var r = lastRecords[i];
        if (filter !== 'ALL' && r.status !== filter) continue;
        var tr = document.createElement('tr');
        var cols = [
          r.waybillNo,
          r.status,
          r.stage,
          r.needReprint ? '是' : '-',
          r.mobileUpdated ? '是' : '-',
          r.errorMsg || r.notice || '成功',
        ];
        for (var c = 0; c < cols.length; c++) {
          var td = document.createElement('td');
          td.textContent = cols[c];
          if (c === 1) {
            if (r.status === STATES.DONE) td.style.color = '#16a34a';
            else if (r.status === STATES.FAILED) td.style.color = '#dc2626';
            else if (r.status === STATES.SKIPPED) td.style.color = '#d97706';
          }
          tr.appendChild(td);
        }
        tbody.appendChild(tr);
      }
    }

    $('ra-tab-log').addEventListener('click', function () {
      $('ra-tab-log').classList.add('active');
      $('ra-tab-tbl').classList.remove('active');
      $('ra-log-box').style.display = 'block';
      $('ra-table-wrap').style.display = 'none';
      $('ra-tbl-filter').style.display = 'none';
    });
    $('ra-tab-tbl').addEventListener('click', function () {
      $('ra-tab-tbl').classList.add('active');
      $('ra-tab-log').classList.remove('active');
      $('ra-log-box').style.display = 'none';
      $('ra-table-wrap').style.display = 'block';
      $('ra-tbl-filter').style.display = 'inline-block';
      renderTable();
    });
    $('ra-tbl-filter').addEventListener('change', renderTable);

    // 单号实时解析与文件拖入
    function refreshInputStats() {
      var parsed = parseWaybillInput($('ra-input').value);
      $('ra-parse-info').textContent =
        '已识别 ' + parsed.waybills.length + ' 单（去重 ' + parsed.duplicateCount + ' 条，忽略表头/无效 ' + parsed.invalidCount + ' 行）';
      return parsed;
    }
    $('ra-input').addEventListener('input', refreshInputStats);
    $('ra-btn-clear').addEventListener('click', function () {
      $('ra-input').value = '';
      refreshInputStats();
    });
    $('ra-btn-import').addEventListener('click', function () { $('ra-file').click(); });

    function loadFile(file) {
      if (!file) return;
      var reader = new FileReader();
      reader.onload = function () {
        $('ra-input').value = String(reader.result || '');
        var p = refreshInputStats();
        appendLog({ time: formatTimeHMS(), level: 'info', stage: 'CSV', message: '已导入 ' + file.name + '：有效单号 ' + p.waybills.length + ' 个' });
      };
      reader.readAsText(file, 'utf-8');
    }
    $('ra-file').addEventListener('change', function (e) {
      loadFile(e.target.files && e.target.files[0]);
      e.target.value = '';
    });
    $('ra-input').addEventListener('dragover', function (e) {
      e.preventDefault();
      $('ra-input').classList.add('ra-drag');
    });
    $('ra-input').addEventListener('dragleave', function () {
      $('ra-input').classList.remove('ra-drag');
    });
    $('ra-input').addEventListener('drop', function (e) {
      e.preventDefault();
      $('ra-input').classList.remove('ra-drag');
      if (e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0]) {
        loadFile(e.dataTransfer.files[0]);
      }
    });

    function updateAcctBar(acctText) {
      var el = $('ra-acct-text');
      if (el) el.textContent = acctText;
    }

    // 自动识别当前登录账号（绑定道段 + 投递员）并拉取候选列表
    async function detectAndPopulateAccount(isAutoInit) {
      var btn = $('ra-btn-fetch');
      btn.disabled = true;
      var eng = new ReturnAutoEngine(readConfigFromUI());
      try {
        if (!isAutoInit) {
          appendLog({ time: formatTimeHMS(), level: 'info', stage: 'INIT', message: '正在识别当前登录账号与系统绑定信息...' });
        }
        var detected = null;
        try {
          detected = await eng.detectCurrentAccount();
        } catch (e1) {
          appendLog({ time: formatTimeHMS(), level: 'warn', stage: 'INIT', message: '读取下段账号绑定信息失败：' + e1.message });
        }

        var couriers = [];
        try {
          couriers = await eng.fetchCouriers();
        } catch (e2) {}

        var segs = [];
        try {
          segs = await eng.fetchRoadSegs();
        } catch (e3) {}

        // 填充道段候选（高级参数用）
        var segMap = Object.create(null);
        var segList = $('ra-roadseg-list');
        segList.innerHTML = '';
        var allSegs = (detected && detected.roadsegs ? detected.roadsegs : []).concat(segs);
        allSegs.forEach(function (s) {
          if (!s || !s.code || segMap[s.code]) return;
          segMap[s.code] = true;
          var opt = document.createElement('option');
          opt.value = s.code;
          opt.label = s.name || s.code;
          segList.appendChild(opt);
        });

        // 填充投递员候选
        var cList = $('ra-courier-list');
        cList.innerHTML = '';
        couriers.forEach(function (name) {
          var opt = document.createElement('option');
          opt.value = name;
          cList.appendChild(opt);
        });

        // 同步当前登录账号绑定的道段与投递员（支持换账号登录自动切换）
        var matchedCourier = '';
        if (detected && detected.dlvPersonName) {
          matchedCourier = detected.dlvPersonName;
          for (var i = 0; i < couriers.length; i++) {
            if (couriers[i] === detected.dlvPersonName || (detected.staffId && couriers[i].indexOf(detected.staffId) !== -1)) {
              matchedCourier = couriers[i];
              break;
            }
          }
          $('ra-courier').value = matchedCourier;
        }
        if (detected && detected.roadseg) {
          $('ra-roadseg').value = detected.roadseg;
        } else if (!$('ra-roadseg').value && segs.length > 0) {
          $('ra-roadseg').value = segs[0].code;
        }

        var curRoadseg = $('ra-roadseg').value.trim();
        var curCourier = $('ra-courier').value.trim();
        readConfigFromUI();

        if (curCourier || curRoadseg) {
          updateAcctBar(
            '当前账号：' +
              (curCourier || '请下拉选择投递员') +
              ' | 系统绑定道段：' +
              (curRoadseg || '待识别') +
              (detected && detected.roadseg ? '（自动识别）' : '')
          );
          appendLog({
            time: formatTimeHMS(),
            level: 'ok',
            stage: 'INIT',
            message:
              '账号识别完成：投递员=' +
              (curCourier || '未识别') +
              '，系统绑定道段=' +
              (curRoadseg || '未识别') +
              '（无需手动填道段）',
          });
        } else {
          updateAcctBar('未能自动识别账号，请确认已登录内网后点击右侧按钮重试');
        }
      } catch (err) {
        updateAcctBar('识别账号失败，请确认登录状态后重试');
        appendLog({ time: formatTimeHMS(), level: 'error', stage: 'INIT', message: '账号识别失败：' + err.message });
      } finally {
        btn.disabled = false;
      }
    }

    $('ra-btn-fetch').addEventListener('click', function () {
      detectAndPopulateAccount(false);
    });

    // 启动 / 暂停 / 中止 / 导出
    $('ra-btn-start').addEventListener('click', async function () {
      var parsed = refreshInputStats();
      var runCfg = readConfigFromUI();
      var eng = new ReturnAutoEngine(runCfg, {
        onLog: appendLog,
        onProgress: function (p) {
          updateSummaryUI(p.summary, p.phaseLabel, p.current, p.total);
        },
        onStateChange: function (st) {
          var busy = st === 'running' || st === 'paused';
          $('ra-btn-start').disabled = busy;
          $('ra-btn-pause').disabled = !busy;
          $('ra-btn-pause').textContent = st === 'paused' ? '继续' : '暂停';
          $('ra-btn-stop').disabled = !busy;
        },
      });
      var errs = eng.validateConfig(parsed.waybills);
      if (errs.length > 0) {
        appendLog({ time: formatTimeHMS(), level: 'error', stage: '校验', message: errs.join('；') });
        window.alert(errs.join('\n'));
        return;
      }
      if (!runCfg.dryRun) {
        var ok = window.confirm(
          '即将以【正式模式】处理 ' +
            parsed.waybills.length +
            ' 单！\n归班投递员：' +
            (runCfg.dlvPersonName || '(自动识别)') +
            '\n系统绑定道段：' +
            (runCfg.roadseg || '(运行时自动识别)') +
            '\n补打固定手机号：' +
            runCfg.fixedMobile +
            '\n确定开始正式提交？'
        );
        if (!ok) return;
      }

      $('ra-log-box').innerHTML = '';
      activeEngine = eng;
      var result = await eng.run(parsed.waybills);
      lastRecords = result.records || [];
      renderTable();
      var failList = lastRecords.filter(function (r) { return r.status === STATES.FAILED; });
      $('ra-btn-csv').disabled = lastRecords.length === 0;
      $('ra-btn-copy-fail').disabled = failList.length === 0;
      $('ra-btn-retry-fail').disabled = failList.length === 0;
    });

    $('ra-btn-pause').addEventListener('click', function () {
      if (!activeEngine || !activeEngine.running) return;
      if (activeEngine.paused) activeEngine.resume();
      else activeEngine.pause();
    });

    $('ra-btn-stop').addEventListener('click', function () {
      if (activeEngine && activeEngine.running) activeEngine.stop();
    });

    $('ra-btn-csv').addEventListener('click', function () {
      if (!lastRecords.length) return;
      var fname = '退货处理报表_' + formatDateYMD() + '_' + formatTimeHMS().replace(/:/g, '') + '.csv';
      triggerDownload(fname, toCsv(lastRecords));
    });

    $('ra-btn-copy-fail').addEventListener('click', function () {
      var txt = lastRecords.filter(function (r) { return r.status === STATES.FAILED; }).map(function (r) { return r.waybillNo; }).join('\n');
      if (!txt) return;
      copyText(txt).then(function () {
        appendLog({ time: formatTimeHMS(), level: 'ok', stage: 'SYS', message: '已复制全部失败单号到剪贴板' });
      });
    });

    $('ra-btn-retry-fail').addEventListener('click', function () {
      var txt = lastRecords.filter(function (r) { return r.status === STATES.FAILED; }).map(function (r) { return r.waybillNo; }).join('\n');
      if (!txt) return;
      $('ra-input').value = txt;
      refreshInputStats();
      appendLog({ time: formatTimeHMS(), level: 'info', stage: 'SYS', message: '已将失败单号重新载入输入框' });
    });

    var hasAutoDetected = false;
    rootEl.__raOnOpen = function () {
      if (!hasAutoDetected && onIntranetApp) {
        hasAutoDetected = true;
        detectAndPopulateAccount(true);
      }
    };

    appendLog({ time: formatTimeHMS(), level: 'info', stage: 'SYS', message: '面板已就绪（默认演练模式）。直接粘贴单号或拖入 CSV 即可测试运行。' });
    if (opts.startHidden) {
      rootEl.style.display = 'none';
      if (fabBtn) fabBtn.style.display = 'inline-flex';
    } else if (onIntranetApp) {
      hasAutoDetected = true;
      detectAndPopulateAccount(true);
    } else {
      updateAcctBar('当前不在内网业务页，登录 /portal/ 或 /delivery-web/ 后即可自动识别账号');
    }
    return rootEl;
  }

  return {
    VERSION: VERSION,
    STATES: STATES,
    DEFAULT_CONFIG: DEFAULT_CONFIG,
    detectCtx: detectCtx,
    isIntranetContext: isIntranetContext,
    formatDateYMD: formatDateYMD,
    formatTimeHMS: formatTimeHMS,
    chunkArray: chunkArray,
    parseWaybillInput: parseWaybillInput,
    needsMobileUpdate: needsMobileUpdate,
    isValidMobile: isValidMobile,
    decodeModelContent: decodeModelContent,
    parseSetRoadSegHtml: parseSetRoadSegHtml,
    toCsv: toCsv,
    encodeForm: encodeForm,
    SessionExpiredError: SessionExpiredError,
    createXhrTransport: createXhrTransport,
    ReturnAutoEngine: ReturnAutoEngine,
    mountPanel: mountPanel,
    togglePanel: togglePanel,
  };
});
