# 小程序集成夹具

[运行器](../../tests/http/native/runner.ts)为原生小程序提供有限测试环境。它启动真实 `Application::start_with_mini`、临时 PostgreSQL 18.4 和回环 Mini 供应商替身。所有账户、手机号、供应商输入和凭据均为虚构材料。业务合同仍由[小程序手机号](../identity/mini-mobile.md)和[退出](../identity/revocation.md)拥有。

## 运行边界

夹具只监听 `127.0.0.1`。`--service-origin` 必须是精确的 `https://127.0.0.1:<port>`，端口范围为 `1` 至 `65535`，不接受路径、用户信息、查询或片段。客户端使用这个 HTTPS origin，dev 在相同端口提供回环 HTTP。TLS、转发、证书和微信域名资格由原生验收宿主负责。夹具不改变微信安全设置。

转发器剥离客户端提供的来源头，并设置固定虚构地址 `203.0.113.77`。所有请求共用正式限流预算。供应商替身只访问回环地址。`a-` 开头的虚构 code 使用账户 A 的号码。`b-` 开头的 code 使用账户 B 的号码。每次新登录采用新的虚构 code。重复 code 保留正式占用规则。

头像链接使用任务 HTTPS origin 和正式签名路径。自证适配器把这个 origin 映射至回环 HTTP，实际读取头像字节。自证不访问示例公网地址。

真实应用报告 ready 后开始固定 15 分钟业务窗口。私有控制须在 120 秒内连接。Rust 排空上限另为 150 秒，控制连接收尾有 5 秒上限。监督器最多再等待进程 150 秒。超时后分别给予 SIGTERM 10 秒、SIGKILL 5 秒。强制终止、未知身份或未确认清理都会使运行失败。

每次运行创建一个 `0700` 私有目录。`owner.json` 绑定任务、用途和 run，`input.json` 绑定后端来源、OpenAPI 摘要及正式客户端输入。`ready.json` 提供实际地址和 Unix socket。各 `checkpoint-<case>.json` 保存阶段绑定、执行前投影及独立事实原件。`evidence.json` 保存最终业务事实与 Rust 收尾结果，`runner.json` 保存实际进程退出和独立 PG 读回，`supervisor.log` 保存脱敏诊断。

业务事实限于固定查询投影的 ID、计数、状态、时间和布尔值。事实接口不返回 token、凭据摘要、密钥、手机号或密文。源码与 OpenAPI 摘要只用于来源绑定。

## 非原生自证

使用仓库固定的 Bun、Node 和 Rust。先按 [Rust 验证](rust.md#workspace-validation)准备 PostgreSQL 与锁定依赖。运行器核对 `.tool-versions`，并以 `--locked --offline` 构建 HTTP 测试。

以下命令从仓库根目录执行。将 `CLIENT_COMMIT` 替换为本次正式发布或合并的完整 Git 修订，将 `TASK_OWNER` 替换为实际任务标识。

```sh
export CLIENT_COMMIT=<full-git-commit>
export TASK_OWNER=<chat-or-task-id>
export CARGO_HOME="$PWD/target/cargo"
export CARGO_NET_OFFLINE=true
node orbit/tools/protocol.ts
node node_modules/typescript/bin/tsc -p tests/http/native/tsconfig.json --noEmit
bun --no-env-file tests/http/native/runner.ts --owner "$TASK_OWNER" --client-revision "$CLIENT_COMMIT" --mode full
NATIVE_SELFTEST_CLIENT_REVISION="$CLIENT_COMMIT" bun --no-env-file test tests/http/native/lifecycle.test.ts
```

运行器从正式 Git 修订导出只读客户端输入，在该输入中运行原协议生成器。运行器核对客户端依赖锁与已准备依赖一致。随后加载该修订的真实 Controller 和 Storage。导出目录中的依赖链接指向现有缓存，不修改原客户端。源码修订、生成结果和运行材料分别记录。

`full` 使用最小平台适配器发送真实 HTTP，并通过正常 Storage 保存材料。场景覆盖 R2、Other、Current、All 各一次已提交成功响应丢失。Current 和 All 通过后续有效登录读取正式原结果。读取 `503` 的组合从真实退出留下的有效恢复材料开始，核对恢复入口与材料保留。列表字段、排序、当前项、来源、跨账户隔离、签名头像及供应商调用次数也在该场景核对。

生命周期回归覆盖正常停止、EOF、启动失败、两种信号、截止与未确认 PG 停止。短截止和清理中断使用专用 Rust 测试入口；原生入口始终使用 15 分钟窗口。清理中断会实际保留 PG。监督器只对身份已核实的自有实例执行有界兜底，并保持原运行失败。

非原生自证只证明夹具与实际客户端代码的组合。原生验收另核真实 `wx.request`、页面、Storage、平台资格和连接路径。真实微信供应商弹窗另有验收范围。

## 原生前台运行

原生负责人确认连接方案和平台授权后，使用 `external`。`CLIENT_BUILD` 绑定已核实的原生包；省略时记录源码自证构建标识。业务请求、控制和事实各走原有入口，控制 socket 不向 HTTP 暴露。

```sh
bun --no-env-file tests/http/native/runner.ts \
  --owner "$TASK_OWNER" \
  --client-revision "$CLIENT_COMMIT" \
  --client-build "$CLIENT_BUILD" \
  --service-origin https://127.0.0.1:9443 \
  --mode external
```

监督器自动完成 `hello`。随后通过同一前台进程的 stdin 逐行提交 JSON。每行上限为 64 KiB；未知字段、其他 run 和乱序命令均被拒绝。所有命令的 `run` 使用本次 ready 中的 UUID。关闭 stdin 会走 EOF 收尾。

| 命令         | 输入与作用                                                                                                      |
| ------------ | --------------------------------------------------------------------------------------------------------------- |
| `snapshot`   | 读取独立连接的固定业务投影及供应商计数                                                                          |
| `arm`        | 绑定一个有限故障阶段；参数为 `case`、`operation`、`actor`、`target`、`registration`                             |
| `checkpoint` | 核对该阶段的原操作与固定事实；参数为 `case`、`operation`、`actor`、`target`、`later_session`、`foreign_session` |
| `finish`     | 提交已观察的结果并排空；完整业务结果要求全部故障和观察通过                                                      |
| `abort`      | 以 `operator_stop`、`client_failure`、`client_timeout` 或 `control_failure` 原因停止                            |

`registration` 故障限定 POST `/api/v1/registrations`。已知操作时填写准确 `operation`；客户端尚未生成操作时，先填写真实 R1 返回的 `registration` ID。退出故障分别为 `logout_current`、`logout_all`、`logout_other`，绑定真实 `actor`；Other 还绑定 `target`。已知操作时使用准确 `operation`；未知操作只在该有限阶段原子捕获首个匹配操作。`current_503`、`sessions_503` 绑定真实 actor 和对应 GET 路径。

同一 run 的每种故障只允许一次。前一阶段完成 `checkpoint` 后，才能装载下一阶段。错误路径、actor、Registration 或明确操作都不会消耗故障。丢弃响应前，独立数据库连接先确认原操作已经持久完成。转发器不自动重试写入。

`checkpoint` 使用已捕获的原操作。填写 `operation` 时，必须与原操作一致。Current 和 All 填写后续同账户的 `later_session`，Other 填写其他账户的 `foreign_session`。事实核对成功并保存原件后，该阶段才释放。

`finish.observations` 的封闭字段见[控制定义](../../tests/http/native/control.rs)。操作者只提交本次实际观察。空场景停止报告 `stopped`；已有故障但缺少完整证据报告失败。夹具完成不代替原生领域负责人接纳。

## 清理读回

正常停止先关闭入口。随后等待转发、Application、观测连接和供应商终态，再停止 PG。PG 所有者核对主进程身份、监听器和目录清理。监督器另从 `/proc` 核对可执行文件、数据目录、启动身份和实际残留。

监督器在正常清理已确认后移除本次 scratch。失败或未知结果保留目录和诊断。兜底只作用于本次私有目录内、身份核实的 PG，兜底成功仍记录失败。保留材料的后续回收遵守[验证边界](verification.md#检查范围)和原任务授权。
