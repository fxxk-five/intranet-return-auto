# intranet-return-auto（内网退货自动处理程序）

面向内网投递业务 Web 系统的**离线退货全流程自动化工具**与配套 **Rust CDP 诊断采集探针**。

> 所有示例代码、测试夹具与文档中的姓名、工号、运单号、手机号与地址均已脱敏为虚拟测试数据，仓库不包含任何内网回传数据、Cookie/会话凭据或真实业务记录。

---

## 目录结构

| 目录 | 说明 |
|---|---|
| [`automation/`](./automation/) | **正式自动化交付物**：浏览器单文件脚本（`return_auto.js`）、离线 Edge MV3 本地扩展（`edge-extension/` 与 `edge-extension.zip`）、Node.js 离线自测套件（`test_return_auto.js`）及使用手册 |
| [`probe/`](./probe/) | Rust 编写的零依赖 Windows Edge CDP 流程采集与源码镜像探针（用于前期离线环境调研与契约提取） |
| [`tools/`](./tools/) | `probe` 配套的端到端模拟与回归校验脚本 |

---

## 快速开始（内网 Edge 离线安装）

详细步骤见 [`automation/README.md`](./automation/README.md)：

1. 将 [`automation/edge-extension/`](./automation/edge-extension/)（或解压 [`automation/edge-extension.zip`](./automation/edge-extension.zip)）拷入内网电脑固定目录。
2. 在 Edge 打开 `edge://extensions` -> 开启 **开发人员模式** -> 点击 **加载解压缩的扩展** -> 选择 `edge-extension` 目录。
3. 登录内网业务门户（`/portal/a` 或 `/delivery-web/`），点击页面右下角蓝色 **「退货助手」** 悬浮按钮（或浏览器工具栏图标）即可打开控制面板：
   - 自动识别当前登录账号的投递员与系统绑定道段（免手填道段）
   - 内置 **A（下段预检）→ B（撤单自动补打与补手机号，零出纸）→ A（回流重扫与批量下段）→ C（退回妥投）→ D（归班交接，严格交集防误交接）** 条件分流状态机
   - 默认开启 **演练模式**（只读预检，零写入），支持暂停/继续/中止与防科学计数法 CSV 报表导出

---

## 本地离线自测

### 1. 自动化脚本与 Edge 扩展测试（Node.js 18+）

```powershell
node --check automation/return_auto.js
node automation/test_return_auto.js
```

### 2. Rust 探针单元测试

```bash
cd probe
cargo test
```
