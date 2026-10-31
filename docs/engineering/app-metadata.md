# Cladiron App 与安装元数据

[独立入口](../../.github/review-approval/app-metadata.ts)核实现有凭据对应的 App 和安装身份。[手动工作流](../../.github/workflows/app-metadata.yml)与批准链分开，不调用 installation token、批准或撤回逻辑。批准证据继续由[机器合同](review-approval.md)拥有。

## 目标与结果

一次成功读取只访问 `api.github.com` 的三个固定目标，顺序如下：

1. `GET /app`：App `3104501`、slug `cladiron`、owner `19648989`，以及对应 client ID。
2. `GET /app/installations/175475891`：同一 App、账号与 installation；仓库选择为 `selected`，安装未暂停。
3. `GET /repos/headironc/nucleus/installation`：返回同一 installation，并核对相同的受核权限投影。

仓库端点返回安装对象，不返回仓库对象。结果中的仓库数字 ID `1432619931` 来自受信 Actions 上下文守卫，`idSource` 明确保存来源。成功的仓库端点证明名称对应的安装关联，不独立证明仓库数字 ID。

权限结果只报告 `metadata` 和 `pull_requests` 的实际等级。缺失项记为 `null`，其他权限只计数。两次安装响应的投影不同就拒绝，不拼接冲突读数。该投影不是全部权限清单，也不是 installation token 的实际最小作用域。`selected` 和仓库关联成功不证明安装总共只有一个仓库。

结果只包含固定目标、安全元数据和来源。来源记录 workflow、run、attempt、执行 revision 与观察时间。JWT、私钥和提供方额外字段不进入结果。三个读取分别发生，不宣称它们构成原子快照。

## 执行与秘密边界

真实运行前，先完成源码发布、受信 bootstrap 接纳及具体执行授权。新工作流和能力目录的变化仍受[执行输入冻结](review-approval.md#ci-receipt)约束。工作流存在、模拟通过或源码合并都不代表已经启用或执行。当前交付状态只由[能力状态](../README.md#能力状态)记录。

入口只接受所有者从同仓库 `dev` 手动运行。分别核对初次 actor 和 triggering actor。仓库名称、数字 ID 和 owner 都先校验。workflow ref、revision、run 和 attempt 也先校验。它只检出 `github.workflow_sha`，不执行 PR 源码。结果绑定执行修订，不额外查询或保证执行期间 `dev` 始终未变。

作业使用 `dev-admission` 环境。只有读取步骤引用现有 `vars.CLADIRON_CLIENT_ID` 和 `secrets.NUCLEUS_DEV_PRIVATE_KEY`。标准 Actions token 仅保留 checkout 所需的 `contents: read`。该 token 不传给读取程序。秘密的实际解析层级沿 GitHub 的环境与仓库配置，不由本入口改写。授权真实运行前，负责人确认该秘密来源和目标。

程序通过 `bun --no-env-file .github/review-approval/app-metadata.ts` 启动。环境身份守卫先于凭据读取。私钥和 JWT 仅在当前进程内使用，不保存、输出或返回。标准 crypto 只签一次 RS256 JWT，三次读取复用该 JWT。`iat` 回退 60 秒，`exp` 为签发时钟后 540 秒。该字节规则由[具体签名能力](../../.github/review-approval/app-jwt.ts)与已有 mint 消费者共用，凭据来源和错误处理各自拥有。

执行完成后不再使用 JWT。程序不承诺清零运行时字符串；进程退出结束本次内存使用，JWT 自身仍受 `exp` 限制。入口不创建 installation token，也没有 token 撤销步骤。

## 失败与回读

每个 GET 最多发一次，超时为 30 秒。`redirect: manual` 保留实际 HTTP 状态；所有非 `200` 都停止，不跟随重定向，也不读取 `Location` 或错误正文。`401`、`403`、`301` 和 `404` 都保留对应阶段与状态，不自动转换为“未安装”。

网络结果未知、超时、无效正文或身份不符都失败。程序不重试、不重签 JWT，也不进入批准的失败撤回链。安全错误只输出固定来源、操作阶段、原因和已知 HTTP 状态。原始异常及提供方文本不进入日志。

负责人从同一 run/attempt 的步骤和 JSON 日志回读结果。读取失败时保留准确阶段和未知状态。另一次运行按有效授权重新决定。不把失败当成许可已撤销或自动恢复的依据。元数据成功不证明已获得 PR 写权限，也不替代真实批准、撤回和恢复验收。

## 本地验证

使用内存生成的合成 RSA，完全替换 transport。测试检查精确三个 GET、签名与时限、身份和安装拒绝及权限投影。另验各阶段的 HTTP、超时和未知失败，以及 CLI 安全输出。默认网络边界也替换为拒绝函数。CLI 子进程只传合成环境并 preload 替身，不自动加载 `.env`。合成私钥不写文件。

在仓库固定 Bun 环境执行：

```sh
./node_modules/.bin/tsc -p .github/review-approval/tsconfig.json --noEmit
bun --no-env-file test ./.github/review-approval/*.test.ts
```

工作流语法和文档检查分别执行。测试临时目录沿 task-temp 生命周期清理；真实身份结果与无秘密模拟分开验收。

## 官方接口依据

App 响应形状见 GitHub 的 [App 元数据](https://docs.github.com/en/rest/apps/apps#get-the-authenticated-app)。安装形状与状态见 [installation 元数据](https://docs.github.com/en/rest/apps/apps#get-an-installation-for-the-authenticated-app)和[仓库安装关联](https://docs.github.com/en/rest/apps/apps#get-a-repository-installation-for-the-authenticated-app)。JWT 的算法、issuer 和时间约束见[官方签名说明](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app)。本入口沿已有消费者固定使用 `2022-11-28` API 版本。
