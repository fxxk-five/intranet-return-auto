'use strict';

const assert = require('node:assert/strict');
const path = require('node:path');
const api = require(path.join(__dirname, 'return_auto.js'));

const {
  STATES,
  isIntranetContext,
  parseWaybillInput,
  needsMobileUpdate,
  isValidMobile,
  decodeModelContent,
  parseSetRoadSegHtml,
  toCsv,
  SessionExpiredError,
  ReturnAutoEngine,
} = api;

function makeRoadsegObj(wb, overrides) {
  return Object.assign(
    {
      alarmFlag: '',
      receiverName: '张*',
      receiverLinker: '',
      receiverPhone: '',
      receiverAddr: '示例省示例市示例区测试工业园100号',
      dlvOrgId: '',
      dlvOrgName: '',
      dlvOrgNo: '',
      dlvPersonId: '',
      dlvPersonName: '',
      waybillNo: wb,
      isSocial: '',
      codAmount: 0.0,
      mainWaybillNo: '',
      subWaybillNo: '',
      oneBillTotalNum: 0,
      itemType: '快递包裹',
      bizproductno: '11312',
      bizproductno2: '1131200001',
      remark: '0',
      roadSegRes: 1,
      ifPay: '0',
      ifValuable: false,
      dlvRoadSeg: '',
      oneBillFlag: '',
      scanFlag: '',
      ifSet: '',
      resCode: '',
      resMsg: '20',
      comment: '',
      innerHtml: '',
      importantRemarks: '',
    },
    overrides || {}
  );
}

function createMockServer(seedParcels, opts) {
  const options = opts || {};
  const parcels = new Map();
  for (const p of seedParcels) {
    parcels.set(p.waybillNo, Object.assign({ printed: false, setRoadSeg: false, signed: false, returned: false }, p));
  }

  const calls = [];
  let d1Count = 0;
  let transientFailLeft = options.transientA1Fails || 0;

  async function transport(url, params) {
    const ep = url.replace(/^.*\/delivery\//, '');
    calls.push({ ep, url, params: Object.assign({}, params) });

    if (options.expireAtCall && calls.length === options.expireAtCall) {
      throw new SessionExpiredError('模拟登录过期');
    }

    if (ep === 'setroadseg/main') {
      return [
        '<input id="roadsegcode" type="hidden" value="T01"></input>',
        '<input id="roadsegname" type="hidden" value="T01"></input>',
        '<input id="segtype" type="hidden" value="single"></input>',
        '<div id="singlestaff"><label>投递员: <input id="staff" type="hidden">71001234567&nbsp; 张三</input></label></div>',
      ].join('\n');
    }

    if (ep === 'returnwork/getroadsegno') {
      return [
        { dlvRoadSeg: 'T01', dlvRoadsegName: 'T01' },
        { dlvRoadSeg: 'T02', dlvRoadsegName: 'T02' },
      ];
    }

    if (ep === 'returnwork/queryname') {
      return { text: ['张三-71001234567', '李四-71009999999'], errorMsg: '' };
    }

    if (ep === 'setroadseg/queryiteminfo') {
      // 校验必须包含 HAR 实测的 11 个字段
      for (const k of ['namelist', 'alarmFlag', 'waybillNo', 'type', 'roadseg', 'roadsegname', 'societyFlag', 'position', 'tongqian', 'addValueFlag', 'stackFlag']) {
        assert.ok(Object.prototype.hasOwnProperty.call(params, k), 'queryiteminfo 缺少字段 ' + k);
      }
      if (transientFailLeft > 0) {
        transientFailLeft--;
        throw new Error('模拟临时网络抖动');
      }
      const p = parcels.get(params.waybillNo);
      if (!p) {
        return { resCode: 'B00001', resMsg: '投递库无此邮件', comment: 'itemNotExist', waybillNo: '' };
      }
      if (p.kind === 'noPostingInfo') {
        return {
          resCode: 'B00001',
          resMsg: '该邮件无收寄信息,不允许进行投递作业,只能转发回本地处理中心处理。',
          comment: 'noPostingInfo',
          waybillNo: '',
        };
      }
      if (p.kind === 'examineInfo') {
        return { resCode: 'B00001', resMsg: '需稽核', comment: 'examineInfo', waybillNo: '' };
      }
      if (p.kind === 'alreadySet') {
        return { resCode: 'B00001', resMsg: '该邮件已下段，无需重复下段', comment: '', waybillNo: '' };
      }
      if (p.kind === 'reprint' && !p.printed) {
        return {
          resCode: 'B00001',
          resMsg: `该邮件【${p.waybillNo}】为客户撤单邮件，请通过【详情单补打】打印面单后，进行退件封发`,
          comment: '',
          waybillNo: '',
        };
      }
      if (p.kind === 'cod') {
        return makeRoadsegObj(p.waybillNo, { codAmount: 88.5, ifPay: '1' });
      }
      if (p.kind === 'oneBillIncomplete') {
        return makeRoadsegObj(p.waybillNo, {
          oneBillFlag: '1',
          mainWaybillNo: 'MAIN000000001',
          oneBillTotalNum: 3,
        });
      }
      if (p.kind === 'changeAddr') {
        return makeRoadsegObj(p.waybillNo, { resMsg: '30', receiverAddr: '示例市高新区新址88号' });
      }
      return makeRoadsegObj(p.waybillNo);
    }

    if (ep === 'waybillprint/query') {
      const nos = String(params.waybillNoList || '').split(',').filter(Boolean);
      const detail = [];
      for (const wb of nos) {
        const p = parcels.get(wb);
        if (p) {
          detail.push({
            waybillNo: wb,
            receiverMobile: p.receiverMobile !== undefined ? p.receiverMobile : '138****0000',
            receiverSafetyCode: '',
            bizProductNo: p.bizProductNo || '11312',
            bizProductName: '快递包裹',
            printFlag: p.printed ? '1' : '0',
            receiptFlag: '1',
          });
        }
      }
      return { resCode: 'A00001', resMess: '成功', comment: '', detail };
    }

    if (ep === 'waybillprint/queryModel') {
      if (params.bizProductNo === '11210') {
        return [{ templateCode: 'SD_EMS_MD', templateName: '标准EMS面单' }];
      }
      return [{ templateCode: 'SD_GN_KB_MD', templateName: '国内快包面单-正式' }];
    }

    if (ep === 'waybillprint/modelContent') {
      // 模拟真实 HAR 返回：外层带 JSON 双引号、内部转义 \" 且保留 &quot;
      return JSON.stringify(
        `LODOP.SET_LICENSES("示例物流集团公司","KEY");LODOP.PRINT_INITA(0,-37,800,600,&quot;${params.templateCode}&quot;);`
      );
    }

    if (ep === 'waybillprint/updatemobile') {
      assert.match(params.receiverMobile, /^1\d{10}$/, 'updatemobile 必须为 11 位手机号');
      const p = parcels.get(params.waybillNo);
      if (p) p.receiverMobile = params.receiverMobile;
      return { resCode: '', resMess: '', comment: '', detail: [], innerHtml: '', dlvOrgAttribute: '' };
    }

    if (ep === 'waybillprint/print3') {
      for (const k of ['printModel', 'model', 'waybillNo', 'bizProductNo1', 'updateFlag', 'printType', 'printradio1', 'printradio2', 'printradio3', 'printradio4', 'insertSensitive']) {
        assert.ok(Object.prototype.hasOwnProperty.call(params, k), 'print3 缺少字段 ' + k);
      }
      assert.ok(params.model.startsWith('LODOP.SET_LICENSES("示例物流集团公司"'), 'model 应完成外层 JSON 解码');
      assert.ok(params.model.includes('&quot;'), 'model 内部 &quot; 应保持原样');
      const nos = String(params.waybillNo || '').split(',').filter(Boolean);
      const detail = [];
      for (const wb of nos) {
        const p = parcels.get(wb);
        if (p) {
          p.printed = true;
          detail.push({ waybillNo: wb, printModel: params.model });
        }
      }
      return { resCode: 'A00001', resMess: '成功', comment: [], detail };
    }

    if (ep === 'setroadseg/setbilllist') {
      for (const k of ['namelist', 'maillist', 'type', 'roadseg', 'roadsegname', 'dlvpersonno', 'dlvpersonname', 'societyFlag', 'tongqian', 'carLineValue', 'carLineName', 'exchangeName', 'selfTakeCode', 'isItProvided', 'orgAttribute', 'isConfirm']) {
        assert.ok(Object.prototype.hasOwnProperty.call(params, k), 'setbilllist 缺少字段 ' + k);
      }
      const list = JSON.parse(params.maillist);
      assert.ok(Array.isArray(list) && list.length > 0 && list.length <= 200);
      for (const item of list) {
        const p = parcels.get(item.waybillNo);
        if (p) p.setRoadSeg = true;
      }
      return { resCode: 'A00001', resMsg: '成功' };
    }

    if (ep === 'signwaybillreturned/querydeliverinfomanager') {
      const p = parcels.get(params.waybillNo);
      if (!p || (!p.setRoadSeg && p.kind !== 'alreadySet' && !options.preSetForC)) {
        return { resCode: 'B00001', resMess: '该邮件未下段，不可退回妥投' };
      }
      return {
        resCode: 'A00001',
        resMess: `${params.waybillNo}，成功查询到此邮件相关信息!`,
        comment: { waybillNo: params.waybillNo, returnworkState: '0' },
      };
    }

    if (ep === 'signwaybillreturned/completeconfirmBatch') {
      for (const k of ['waybillNo', 'signPersonName', 'signType', 'signPersonId', 'signPersonIdType', 'agentSignRelation', 'netWorkBrand', 'netWorkCode', 'netWorkName', 'netWorkBrandflag', 'netWorkBrandCode', 'isNophone', 'signTwoPicture', 'signThreePicture', 'signPicture']) {
        assert.ok(Object.prototype.hasOwnProperty.call(params, k), 'completeconfirmBatch 缺少字段 ' + k);
      }
      assert.equal(params.signType, '130');
      const nos = String(params.waybillNo || '').split(',').filter(Boolean);
      for (const wb of nos) {
        const p = parcels.get(wb);
        if (p) p.signed = true;
      }
      return { resCode: 'A00001', resMess: '退件妥投反馈成功', comment: '' };
    }

    if (ep === 'returnwork/querydelivertask') {
      d1Count++;
      const detail = [];
      // 混入一条不属于本次批次的其他邮件，验证不会被误交接
      detail.push({ waybillNo: '9999000000001', dlvState: '1', dlvPersonName: '张三' });
      for (const p of parcels.values()) {
        if (p.signed || options.preSignedForD) {
          // 如果启用 delayedWaybill，则第一次 D1 暂不返回该单，第二次返回
          if (options.delayedD1Waybill === p.waybillNo && d1Count === 1) continue;
          detail.push({ waybillNo: p.waybillNo, dlvState: '1', dlvPersonName: '张三' });
        }
      }
      return { resCode: 'A00001', resMess: 'ok', comment: '', detail };
    }

    if (ep === 'returnwork/acceptdelivertask') {
      const nos = String(params.waybillNo || '').split(',').filter(Boolean);
      const comment = { returnWorkFlag: '1' };
      for (const wb of nos) {
        assert.notEqual(wb, '9999000000001', '绝不能将无关邮件送入 acceptdelivertask!');
        const p = parcels.get(wb);
        if (p) p.returned = true;
        comment[wb] = '1';
      }
      return { resCode: 'A00001', resMess: 'ok', comment };
    }

    throw new Error('未知接口: ' + ep);
  }

  return { transport, calls, parcels };
}

async function runAllTests() {
  const tests = [];
  function test(name, fn) {
    tests.push({ name, fn });
  }

  // 1. 纯函数工具测试
  test('parseWaybillInput 兼容多格式/BOM/Excel公式/去重/表头过滤', () => {
    const raw = [
      '\uFEFF运单号,备注',
      '="9800000000001",退货',
      "'9800000000002",
      '9800000000001 9800000000003;EA123456789CN',
      '无效短文本',
    ].join('\r\n');
    const res = parseWaybillInput(raw);
    assert.deepEqual(res.waybills, ['9800000000001', '9800000000002', '9800000000003', 'EA123456789CN']);
    assert.equal(res.duplicateCount, 1);
    assert.equal(res.invalidCount, 2);
  });

  test('needsMobileUpdate 与 isValidMobile 覆盖缺号/脱敏/长度/固话边界', () => {
    assert.equal(needsMobileUpdate({ receiverMobile: '' }), true);
    assert.equal(needsMobileUpdate({ receiverMobile: '138****0000' }), true);
    assert.equal(needsMobileUpdate({ receiverMobile: '********5199' }), true);
    assert.equal(needsMobileUpdate({ receiverMobile: '13012345' }), true);
    assert.equal(needsMobileUpdate({ receiverMobile: '88888888888' }), true);
    assert.equal(needsMobileUpdate({ receiverMobile: '13000000000' }), false);
    assert.equal(needsMobileUpdate({ receiverMobile: '02988881234' }), false);
    assert.equal(isValidMobile('13000000000'), true);
    assert.equal(isValidMobile('130****0000'), false);
  });

  test('decodeModelContent 正确解码 JSON 外层引号并保留内部 &quot;', () => {
    const raw = JSON.stringify('LODOP.SET_LICENSES("示例物流");LODOP.ADD_PRINT_TEXT(&quot;25mm&quot;);');
    const decoded = decodeModelContent(raw);
    assert.equal(decoded, 'LODOP.SET_LICENSES("示例物流");LODOP.ADD_PRINT_TEXT(&quot;25mm&quot;);');
  });

  test('toCsv 导出含 UTF-8 BOM、防科学计数法且可无损二次导入', () => {
    const csv = toCsv([
      { waybillNo: '9800000000001', status: 'DONE', stage: 'D2', needReprint: true, mobileUpdated: true, errorMsg: '含,逗号和"引号"' },
    ]);
    assert.ok(csv.startsWith('\uFEFF'));
    assert.ok(csv.includes('"=""9800000000001"""'));
    assert.ok(csv.includes('"含,逗号和""引号"""'));
    const reparsed = parseWaybillInput(csv);
    assert.deepEqual(reparsed.waybills, ['9800000000001']);
  });

  test('parseSetRoadSegHtml 兼容单道段隐藏域页面与多道段下拉页面，且零配置可自动识别全链路运行', async () => {
    const singleHtml = `
      <input id="roadsegcode" type="hidden" value="T01"></input>
      <input id="roadsegname" type="hidden" value="T01"></input>
      <input id="segtype" type="hidden" value="single"></input>
      <div id="singlestaff" class="span6">
        <label class="label_note">投递员: <input id="staff" type="hidden">710000000004161&nbsp; 张三</input></label>
      </div>
    `;
    const parsedSingle = parseSetRoadSegHtml(singleHtml);
    assert.equal(parsedSingle.segtype, 'single');
    assert.equal(parsedSingle.roadseg, 'T01');
    assert.equal(parsedSingle.roadsegName, 'T01');
    assert.equal(parsedSingle.staffName, '张三');
    assert.equal(parsedSingle.staffId, '710000000004161');
    assert.equal(parsedSingle.dlvPersonName, '张三-710000000004161');

    // 多道段账号模拟
    const multiHtml = `
      <input id="segtype" type="hidden" value="multi">
      <select id="dlvroadinfo">
        <option value="">--请选择--</option>
        <option value="T02&&城北二段" selected>T02 城北二段</option>
        <option value="T03&&城南三段">T03 城南三段</option>
      </select>
    `;
    const parsedMulti = parseSetRoadSegHtml(multiHtml);
    assert.equal(parsedMulti.segtype, 'multi');
    assert.equal(parsedMulti.roadseg, 'T02');
    assert.equal(parsedMulti.roadsegName, '城北二段');
    assert.equal(parsedMulti.roadsegs.length, 2);

    // 零配置（不传 roadseg 也不传 dlvPersonName）运行完整流程，验证自动 GET setroadseg/main 识别
    const seed = [{ waybillNo: '9841500000001', kind: 'reprint', receiverMobile: '138****0000' }];
    const mock = createMockServer(seed);
    const autoEng = new ReturnAutoEngine(
      { roadseg: '', dlvPersonName: '', delayMs: 0, dryRun: false },
      { transport: mock.transport }
    );
    const res = await autoEng.run(['9841500000001']);
    assert.equal(res.state, 'completed');
    assert.equal(res.summary.done, 1);
    assert.equal(autoEng.config.roadseg, 'T01');
    assert.equal(autoEng.config.dlvPersonName, '张三-71001234567');
    assert.ok(mock.calls.some((c) => c.ep === 'setroadseg/main'));
  });

  test('悬浮面板脚本内全部 DOM ID 与事件绑定 100% 匹配', () => {
    const fs = require('node:fs');
    const src = fs.readFileSync(path.join(__dirname, 'return_auto.js'), 'utf8');
    const htmlIds = new Set([...src.matchAll(/id="(ra-[a-z0-9-]+)"/g)].map((m) => m[1]));
    const refIds = new Set([...src.matchAll(/\$\('(ra-[a-z0-9-]+)'\)/g)].map((m) => m[1]));
    for (const id of refIds) {
      assert.ok(htmlIds.has(id), '面板脚本引用了不存在的 DOM ID: ' + id);
    }
    assert.ok(refIds.size >= 25, '应校验至少 25 个面板控件 ID');
  });

  test('离线 Edge 本地扩展结构、MV3 MAIN 世界注入、图标与同步一致性校验', () => {
    const fs = require('node:fs');
    // 校验顶层门户 /portal/a 与业务页 /delivery-web/ 识别，排除登录页
    assert.equal(isIntranetContext({ pathname: '/portal/a' }), true);
    assert.equal(isIntranetContext({ pathname: '/delivery-web/a/delivery/setroadseg/main' }), true);
    assert.equal(isIntranetContext({ pathname: '/cas/login' }), false);
    assert.equal(isIntranetContext({ pathname: '/other' }), false);

    const extDir = path.join(__dirname, 'edge-extension');
    const manifest = JSON.parse(fs.readFileSync(path.join(extDir, 'manifest.json'), 'utf8'));
    assert.equal(manifest.manifest_version, 3);
    assert.equal(manifest.content_scripts.length, 1);
    const cs = manifest.content_scripts[0];
    assert.equal(cs.world, 'MAIN', '必须运行在 MAIN 世界以经过瑞数 XHR Hook');
    assert.equal(cs.all_frames, false, '应仅在顶层窗口挂载，避免嵌套 iframe 重复面板');
    assert.ok(cs.matches.some((m) => m.includes('/portal/*')));
    assert.ok(cs.matches.some((m) => m.includes('/delivery-web/*')));

    // 校验清单引用的全部文件存在
    for (const rel of [manifest.background.service_worker, ...cs.js, ...Object.values(manifest.icons)]) {
      const full = path.join(extDir, rel);
      assert.ok(fs.existsSync(full), '缺少扩展文件: ' + rel);
    }
    // 校验 PNG 魔数头
    for (const rel of Object.values(manifest.icons)) {
      const buf = fs.readFileSync(path.join(extDir, rel));
      assert.deepEqual([...buf.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    }
    // 校验扩展内 return_auto.js 与主文件字节一致
    const mainBuf = fs.readFileSync(path.join(__dirname, 'return_auto.js'));
    const extBuf = fs.readFileSync(path.join(extDir, 'return_auto.js'));
    assert.ok(mainBuf.equals(extBuf), 'edge-extension/return_auto.js 必须与主脚本完全同步');
  });

  // 2. 完整 A→B→A→C→D 混合场景（含批次切分、补手机号精准触发、安全护栏、D1延迟重试与严格交集）
  test('完整 A→B→A→C→D 真实混合场景与接口契约校验', async () => {
    const seed = [
      { waybillNo: '9841000000001', kind: 'normal' },
      { waybillNo: '9841000000002', kind: 'changeAddr' },
      { waybillNo: '9841000000003', kind: 'reprint', receiverMobile: '138****0000' },
      { waybillNo: '9841000000004', kind: 'reprint', receiverMobile: '' },
      { waybillNo: '9841000000005', kind: 'reprint', receiverMobile: '13812345678' }, // 合法号码，不应触发 updatemobile
      { waybillNo: '9841000000006', kind: 'noPostingInfo' },
      { waybillNo: '9841000000007', kind: 'examineInfo' },
      { waybillNo: '9841000000008', kind: 'cod' },
      { waybillNo: '9841000000009', kind: 'oneBillIncomplete' },
      { waybillNo: '9841000000010', kind: 'alreadySet' },
    ];
    const mock = createMockServer(seed, {
      transientA1Fails: 1,
      delayedD1Waybill: '9841000000005',
    });

    const engine = new ReturnAutoEngine(
      {
        roadseg: 'T01',
        dlvPersonName: '张三-71001234567',
        fixedMobile: '13000000000',
        batchSizeA: 2,
        batchSizeB: 2,
        batchSizeC: 2,
        batchSizeD: 2,
        delayMs: 0,
        dryRun: false,
      },
      {
        transport: mock.transport,
        nowFn: () => new Date('2026-11-26T16:20:00+08:00'),
      }
    );

    const res = await engine.run(seed.map((x) => x.waybillNo));
    assert.equal(res.state, 'completed');
    assert.equal(res.summary.total, 10);
    assert.equal(res.summary.done, 6);       // 001,002,003,004,005,010
    assert.equal(res.summary.reprinted, 3);  // 003,004,005
    assert.equal(res.summary.mobileUpdated, 2); // 仅 003 和 004，005 已合规不修改
    assert.equal(res.summary.skipped, 4);    // 006,007,008,009
    assert.equal(res.summary.failed, 0);

    // 验证 updatemobile 只调了 2 次（003 和 004）
    const mobileCalls = mock.calls.filter((c) => c.ep === 'waybillprint/updatemobile').map((c) => c.params.waybillNo);
    assert.deepEqual(mobileCalls, ['9841000000003', '9841000000004']);

    // 验证 setbilllist 按 batchSizeA=2 拆分了 3 批（5单：001,002,003,004,005；010 已下段不重下）
    const a2Calls = mock.calls.filter((c) => c.ep === 'setroadseg/setbilllist');
    assert.equal(a2Calls.length, 3);

    // 验证 D1 因为 005 延迟出现而自动重查了 2 次，最终全部交接且未误交接 9999000000001
    const d1Calls = mock.calls.filter((c) => c.ep === 'returnwork/querydelivertask');
    assert.equal(d1Calls.length, 2);
    const d2Waybills = mock.calls
      .filter((c) => c.ep === 'returnwork/acceptdelivertask')
      .flatMap((c) => c.params.waybillNo.split(','));
    assert.equal(d2Waybills.length, 6);
    assert.ok(!d2Waybills.includes('9999000000001'));
  });

  // 3. 演练模式绝对零写入保障
  test('演练模式 (dryRun=true) 严格只读预检，绝不调用任何写入接口', async () => {
    const seed = [
      { waybillNo: '9841100000001', kind: 'normal' },
      { waybillNo: '9841100000002', kind: 'reprint', receiverMobile: '138****0000' },
      { waybillNo: '9841100000003', kind: 'noPostingInfo' },
    ];
    const mock = createMockServer(seed);
    const engine = new ReturnAutoEngine(
      { roadseg: 'T01', dlvPersonName: '张三-71001234567', delayMs: 0, dryRun: true },
      { transport: mock.transport }
    );
    const res = await engine.run(seed.map((x) => x.waybillNo));
    assert.equal(res.state, 'completed');
    assert.equal(res.summary.done, 2);
    assert.equal(res.summary.skipped, 1);

    const writeEndpoints = new Set([
      'waybillprint/updatemobile',
      'waybillprint/print3',
      'setroadseg/setbilllist',
      'signwaybillreturned/completeconfirmBatch',
      'returnwork/acceptdelivertask',
    ]);
    const triggeredWrites = mock.calls.filter((c) => writeEndpoints.has(c.ep));
    assert.equal(triggeredWrites.length, 0, '演练模式不应触发任何写入接口');
  });

  // 4. AUTO 模板自动识别多产品分组打印
  test('templateCode=AUTO 时按产品类型分别查模板并分组调用 print3', async () => {
    const seed = [
      { waybillNo: '9841200000001', kind: 'reprint', bizProductNo: '11312', receiverMobile: '13800000001' },
      { waybillNo: '9841200000002', kind: 'reprint', bizProductNo: '11210', receiverMobile: '13800000002' },
    ];
    const mock = createMockServer(seed);
    const engine = new ReturnAutoEngine(
      {
        roadseg: 'T01',
        dlvPersonName: '张三-71001234567',
        templateCode: 'AUTO',
        delayMs: 0,
        dryRun: false,
      },
      { transport: mock.transport }
    );
    const res = await engine.run(seed.map((x) => x.waybillNo));
    assert.equal(res.summary.done, 2);
    const printCalls = mock.calls.filter((c) => c.ep === 'waybillprint/print3');
    assert.equal(printCalls.length, 2);
    assert.deepEqual(
      printCalls.map((c) => c.params.printModel).sort(),
      ['SD_EMS_MD', 'SD_GN_KB_MD']
    );
  });

  // 5. 登录会话失效立即安全中断
  test('登录会话失效 (SessionExpiredError) 立即停止并上报 error 状态', async () => {
    const seed = [
      { waybillNo: '9841300000001', kind: 'normal' },
      { waybillNo: '9841300000002', kind: 'normal' },
    ];
    const mock = createMockServer(seed, { expireAtCall: 2 });
    const engine = new ReturnAutoEngine(
      { roadseg: 'T01', dlvPersonName: '张三-71001234567', delayMs: 0, dryRun: false },
      { transport: mock.transport }
    );
    const res = await engine.run(seed.map((x) => x.waybillNo));
    assert.equal(res.state, 'error');
    assert.equal(mock.calls.filter((c) => c.ep === 'setroadseg/setbilllist').length, 0);
  });

  // 6. 暂停/恢复与中止控制 + 单独跑 D 阶段 + 拉取道段/投递员
  test('暂停/恢复、用户中止、单独运行 D 阶段及静态数据拉取', async () => {
    const seed = [
      { waybillNo: '9841400000001', kind: 'normal' },
      { waybillNo: '9841400000002', kind: 'normal' },
      { waybillNo: '9841400000003', kind: 'normal' },
    ];
    const mock = createMockServer(seed, { preSignedForD: true });

    // 拉取道段/投递员
    const helperEng = new ReturnAutoEngine({}, { transport: mock.transport });
    const segs = await helperEng.fetchRoadSegs();
    const couriers = await helperEng.fetchCouriers();
    assert.equal(segs.length, 2);
    assert.equal(couriers[0], '张三-71001234567');

    // 单独运行 D 阶段（演练模式仍真实执行只读 D1 查询，不执行 D2 写入）
    const dOnlyEng = new ReturnAutoEngine(
      {
        dlvPersonName: '张三-71001234567',
        runStages: { A: false, B: false, C: false, D: true },
        dryRun: true,
        delayMs: 0,
      },
      { transport: mock.transport }
    );
    const dRes = await dOnlyEng.run(['9841400000001', '9841400000002']);
    assert.equal(dRes.state, 'completed');
    assert.equal(dRes.summary.done, 2);
    assert.equal(mock.calls.filter((c) => c.ep === 'returnwork/querydelivertask').length, 1);
    assert.equal(mock.calls.filter((c) => c.ep === 'returnwork/acceptdelivertask').length, 0);

    // 测试中途 pause -> resume
    const prMock = createMockServer(seed);
    let prEngine;
    let sawPaused = false;
    prEngine = new ReturnAutoEngine(
      { roadseg: 'T01', dlvPersonName: '张三-71001234567', delayMs: 0, dryRun: false },
      {
        transport: async (u, p) => {
          const out = await prMock.transport(u, p);
          if (prMock.calls.length === 1) {
            prEngine.pause();
            sawPaused = prEngine.paused;
            setTimeout(() => prEngine.resume(), 25);
          }
          return out;
        },
      }
    );
    const prRes = await prEngine.run(['9841400000001', '9841400000002']);
    assert.equal(sawPaused, true);
    assert.equal(prRes.state, 'completed');
    assert.equal(prRes.summary.done, 2);

    // 测试中途 stop
    const stopMock = createMockServer(seed);
    let stopEngine;
    stopEngine = new ReturnAutoEngine(
      { roadseg: 'T01', dlvPersonName: '张三-71001234567', delayMs: 0, dryRun: false },
      {
        transport: async (u, p) => {
          const out = await stopMock.transport(u, p);
          if (stopMock.calls.length === 1) stopEngine.stop();
          return out;
        },
      }
    );
    const sRes = await stopEngine.run(seed.map((x) => x.waybillNo));
    assert.equal(sRes.state, 'stopped');
    assert.equal(stopMock.calls.length, 1);
  });

  let passed = 0;
  for (const t of tests) {
    try {
      await t.fn();
      passed++;
      console.log(`PASS  ${t.name}`);
    } catch (err) {
      console.error(`FAIL  ${t.name}`);
      console.error(err);
      process.exitCode = 1;
    }
  }
  console.log(`\nSummary: ${passed}/${tests.length} tests passed.`);
}

runAllTests();
