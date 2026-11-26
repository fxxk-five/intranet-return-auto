/* 模拟寄递平台 · 前端逻辑
 *
 * 目的：在无内网环境下，让探针能验证它对真实平台做的每一条判定。
 * 与探针相关的行为都被刻意做成了"真实平台可能的样子"：
 *   - 每次点"确定" → 1 个请求 → 1 个单号（H2 路径）
 *   - ?mode=h1 → 输入框被加上 maxlength=21（H1 路径）
 *   - 打印第一次点弹 alert（js_dialog），第三次点弹一串 DOM 模态框（分支B2）
 *   - 妥投确认前弹 confirm（js_dialog）
 */
(function () {
  'use strict';

  var state = {
    scanCount: 0,
    printClick: 0,
    batchDone: false,
    phoneLoop: 0,
    modalQueue: [],
    modalBusy: false
  };

  var $ = function (id) { return document.getElementById(id); };

  /* ---------------- 标签页（Q4 的 dom_tabs 情形） ---------------- */
  var tabs = Array.prototype.slice.call(document.querySelectorAll('.tab'));
  tabs.forEach(function (t) {
    t.addEventListener('click', function () {
      tabs.forEach(function (x) { x.classList.remove('active'); });
      t.classList.add('active');
      ['A', 'B', 'C', 'D'].forEach(function (k) {
        $('panel-' + k).classList.toggle('hidden', k !== t.dataset.tab);
      });
      setStatus('当前：' + t.textContent.trim());
    });
  });

  function setStatus(s) { $('status').textContent = s; }

  /* ---------------- DOM 模态框 ---------------- */
  function showModal(opts) {
    // 排队，保证一次只显示一个（真实平台的弹窗也是一次一个）
    state.modalQueue.push(opts);
    pumpModal();
  }

  function pumpModal() {
    if (state.modalBusy) return;
    var opts = state.modalQueue.shift();
    if (!opts) return;
    state.modalBusy = true;

    $('modalTitle').textContent = opts.title || '提示';
    $('modalBody').innerHTML = opts.body || '';
    var footer = $('modalFooter');
    footer.innerHTML = '';
    (opts.buttons || [{ text: '确定', value: true }]).forEach(function (b) {
      var btn = document.createElement('button');
      btn.className = 'btn' + (b.primary ? ' primary' : '');
      btn.textContent = b.text;
      btn.addEventListener('click', function () {
        closeModal(b.value);
      });
      footer.appendChild(btn);
    });
    $('modalMask').classList.remove('hidden');
    if (opts.onOpen) opts.onOpen();
  }

  function closeModal(value) {
    $('modalMask').classList.add('hidden');
    state.modalBusy = false;
    var cb = state.modalResult;
    state.modalResult = null;
    if (cb) cb(value);
    // 给下一个弹窗留一帧
    setTimeout(pumpModal, 30);
  }

  $('modalClose').addEventListener('click', function () { closeModal(false); });

  function modalPrompt(opts) {
    return new Promise(function (resolve) {
      state.modalResult = resolve;
      showModal(opts);
    });
  }

  /* ---------------- 网络 ---------------- */
  function post(path, obj) {
    setStatus('请求中 ' + path);
    return fetch(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(obj)
    }).then(function (r) { return r.json(); }).then(function (j) {
      setStatus('完成 ' + path);
      return j;
    });
  }

  /* ---------------- 阶段A 下段扫描 ---------------- */
  // ?mode=h1：把 maxlength=21 加到输入框上，用于验证 H1 判定路径
  if (/[?&]mode=h1\b/.test(location.search)) {
    $('mailNo').setAttribute('maxlength', '21');
    setStatus('H1 模式：mailNo 带 maxlength=21');
  }

  $('btnOk').addEventListener('click', function () {
    var v = $('mailNo').value.trim();
    if (!v) return;
    // 关键：1 次点击 → 1 个请求 → 1 个单号
    post('/api/scan', { mailNo: v }).then(function () {
      state.scanCount++;
      $('scanCount').textContent = String(state.scanCount);
      var li = document.createElement('li');
      li.textContent = v + ' 已录入';
      $('scanList').appendChild(li);
      $('mailNo').value = '';
      showModal({ title: '提示', body: '邮件号码 ' + v + ' 下段扫描成功' });
    });
  });

  $('confirmScan').addEventListener('click', function () {
    post('/api/scan-confirm', { count: state.scanCount }).then(function () {
      showModal({ title: '提示', body: '确认下段完成，共 ' + state.scanCount + ' 个单号' });
    });
  });

  /* ---------------- 阶段B 详情单补打 ---------------- */
  $('btnQuery').addEventListener('click', function () {
    var raw = $('printNos').value.trim();
    var nos = raw ? raw.split(',').map(function (s) { return s.trim(); }).filter(Boolean) : [];
    post('/api/print-query', { mailNos: nos }).then(function () {
      var tbody = $('printTable').querySelector('tbody');
      tbody.innerHTML = '';
      nos.forEach(function (n) {
        var tr = document.createElement('tr');
        tr.innerHTML = '<td><input type="checkbox" class="row-check"></td><td>' + n + '</td><td>已下段</td>';
        tbody.appendChild(tr);
      });
      setStatus('查询到 ' + nos.length + ' 条');
    });
  });

  $('checkAllTop').addEventListener('click', function () {
    var on = $('checkAllTop').checked;
    Array.prototype.forEach.call(document.querySelectorAll('.row-check'), function (c) { c.checked = on; });
  });

  $('btnConfirmPrint').addEventListener('click', function () {
    post('/api/print-confirm', { template: $('template').value }).then(function () {
      setStatus('已提交打印');
    });
  });

  /* 打印按钮：刻意复刻 SOP 里的三种行为
   *   第 1 次 → alert（js_dialog）
   *   第 2 次 → 无弹窗，直接完成（分支 B1）
   *   第 3 次 → 一串 DOM 提示框（分支 B2）
   *   第 4 次起 → 逐个弹"填写收件人手机号"，每次都要再点一次打印
   */
  $('btnPrint').addEventListener('click', function () {
    state.printClick++;
    var n = state.printClick;

    if (n === 1) {
      alert('打印任务已提交（模拟）');
      return;
    }
    if (n === 2) {
      setStatus('打印完成（分支B1：无额外弹窗）');
      return;
    }
    if (n === 3) {
      // 分支B2：一串提示框，需要逐个点 ×
      for (var i = 1; i <= 5; i++) {
        showModal({ title: '提示', body: '第 ' + i + ' 条：该邮件号已完成下段扫描，无需重复打印' });
      }
      return;
    }
    if (n >= 4 && state.phoneLoop < 3) {
      state.phoneLoop++;
      modalPrompt({
        title: '填写收件人手机号',
        body: '<label>收件人手机号 <input id="phoneInput" type="text" value=""></label>',
        buttons: [{ text: '确定', value: true, primary: true }],
        onOpen: function () { setTimeout(function () { $('phoneInput').focus(); }, 10); }
      }).then(function () {
        post('/api/print-phone', { phone: ($('phoneInput') || {}).value || '' }).then(function () {
          setStatus('第 ' + state.phoneLoop + ' 个手机号已提交，请再点一次打印');
        });
      });
      return;
    }
    setStatus('打印流程已完成');
  });

  /* ---------------- 阶段C 退回寄件人 / 妥投确认 ---------------- */
  $('btnReturn').addEventListener('click', function () {
    var v = $('returnMailNo').value.trim();
    if (!v) return;
    post('/api/return', { mailNo: v }).then(function () {
      var li = document.createElement('li');
      li.textContent = v + ' 已退回';
      $('returnList').appendChild(li);
      $('returnMailNo').value = '';
      setStatus('已退回 ' + v);
    });
  });

  $('btnDelivered').addEventListener('click', function () {
    // confirm 也是 js_dialog 的一种
    if (!confirm('确认将选中记录标记为妥投？')) return;
    post('/api/delivered', { count: document.querySelectorAll('#returnList li').length }).then(function () {
      showModal({
        title: '提示',
        body: '妥投确认完成',
        buttons: [{ text: '关闭', value: true, primary: true }]
      });
    });
  });

  /* ---------------- 阶段D 投递归班 ---------------- */
  $('btnHandover').addEventListener('click', function () {
    post('/api/handover', { ts: Date.now() }).then(function () {
      $('panel-D').querySelector('p').innerHTML = '当前批次：<b>已交接</b>';
      setStatus('交班完成');
    });
  });

  $('logout').addEventListener('click', function (e) { e.preventDefault(); setStatus('已退出（模拟）'); });

  // 故意用 eval 造一个"没有 URL 的运行时脚本"：
  // 探针必须能从 Debugger.scriptParsed 里把它捞出来存进 mirror/inline_scripts/
  try {
    eval("window.__mockDynamic = function () { return 'dynamic-script-marker'; };");
  } catch (e) { /* 静默 */ }

  setStatus('就绪');
})();
