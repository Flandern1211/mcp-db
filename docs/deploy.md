# 部署与 CI/CD

本文是 mcp-db 生产部署的操作手册：GitHub Actions 检查并冒烟测试镜像，`main` 推送后发布不可变摘要并部署到 Linux 主机。产品边界见 [SPEC.md](../SPEC.md)，工程约定见 [project-conventions.md](../project-conventions.md)。

仓库和文档不保存任何真实主机地址、账号、密钥或指纹；下文用 `<host>`、`<user>`、`/path/to/mcp-db` 表示由管理员自行填写的值。

## 1. 流水线

| 触发 | `verify` | `publish` | `deploy` |
| --- | --- | --- | --- |
| Pull Request 到 `develop` / `main` | 检查 + 镜像冒烟 | 不运行 | 不运行 |
| 推送到 `develop` | 检查 + 镜像冒烟 | 不运行 | 不运行 |
| 推送到 `main` | 检查 + 镜像冒烟 + 保存镜像产物 | 推送 GHCR 并输出摘要 | 按摘要部署 |
| 手动 `workflow_dispatch` | 检查 + 镜像冒烟 | 不运行 | 不运行 |

- 三个 job 串行依赖：`verify` → `publish` → `deploy`；前一步失败，后面的 job 不运行。
- `verify` 使用只读 `contents` 权限，checkout 设置 `persist-credentials: false`，先跑 `tests/deploy*.test.sh` 的脚本回归，再跑 `bash scripts/verify.sh fast`（固定 Rust 容器内的 fmt、clippy 和 locked 测试），构建 `linux/amd64` 镜像，并用 `tests/deploy-smoke.sh` 做容器冒烟。
- 冒烟使用临时目录中的合成主密钥和干净的临时数据挂载，由 Docker 分配回环临时端口，在有限超时内等待健康检查，然后直接请求 `/healthz` 和 `/admin/login`，两者都必须是 200。不输出业务日志，不连接任何外部系统。
- 只有 `main` 推送才把**冒烟通过的那个镜像** `docker save` 成 artifact 交给 `publish`；发布 job 不 checkout、不重新构建。
- `publish` 只有 `packages: write` 权限，只在 `main` 推送运行；镜像名使用全小写 `ghcr.io/<owner>/mcp-db`，标签是 `sha-<完整提交号>`，不推送 `latest`。构建时写入 OCI `source` 和 `revision` 标签。推送后只输出本次推送的 `ghcr.io/<owner>/mcp-db@sha256:<64 位小写十六进制>` 摘要。
- `deploy` 只在 `main` 推送运行，绑定 `production` environment，job 级并发组 `deploy-production` 且 `cancel-in-progress: false`，不会取消正在进行的部署。它不 checkout，不使用 scp，不向主机上传 compose 文件、脚本或配置，也不在服务器上做 registry 登录；只把摘要交给固定 forced command。
- 手动 `workflow_dispatch` 只做检查和冒烟，**不是部署入口**。需要手工部署时，在主机上用已发布的摘要直接运行 `deploy.sh`（见第 4 节）。
- 部署开始前会确认该提交仍是 `main` 的最新提交；已落后的排队或手动重跑不会发送到主机。

## 2. GitHub 配置

### 2.1 production environment

在仓库 Settings → Environments 新建 `production`：

1. 把 Deployment branches 限制为 `main`，其他分支不能使用该环境。
2. 把下表四个值保存为 **environment secrets**（不是 repository secrets）。工作流只在 `production` job 内引用它们。
3. 可按需增加 required reviewers，让部署在发布后等待人工批准。

| Secret | 内容 |
| --- | --- |
| `DEPLOY_HOST` | 目标主机名或 IPv4 地址，不含端口 |
| `DEPLOY_USER` | 只负责部署的账号 |
| `DEPLOY_SSH_KEY` | 专用部署私钥全文 |
| `DEPLOY_KNOWN_HOSTS` | 目标主机的已知主机公钥行 |

`DEPLOY_KNOWN_HOSTS` 必须来自独立核实的主机指纹（例如云厂商控制台或管理员此前记录的值）。只运行 `ssh-keyscan` 并把结果直接写入 secret 不能证明主机身份。CI 使用 `StrictHostKeyChecking=yes` 和固定的 known_hosts 文件，不会用 `accept-new` 或关闭校验。

### 2.2 GHCR 可见性

目标主机匿名拉取公开镜像，服务器上不保存 `read:packages` 凭据，也不做 registry 登录。

首次发布时 GHCR 包可能默认是 private：`publish` job 仍会成功，但首次 `deploy` 会在 `docker compose pull` 处失败且不切换正在运行的服务。此时在包设置中把可见性改为 public，然后对这一条运行选择 **Re-run failed jobs**，只重跑失败的部署 job；这是首次发布的一次性步骤。若这期间 `main` 又有新提交，旧运行会被“不再指向 main 最新提交”的检查拒绝，直接让新运行完成部署即可。不要改用可变的标签，也不要在服务器上添加认证。

## 3. 主机一次性准备

### 3.1 前提

- Linux amd64，Docker Engine ≥ 24。
- Docker Compose v2，需支持 `up --wait`、`--wait-timeout` 和 `--pull never`（建议 ≥ 2.20）。
- `flock`（util-linux）、`curl`。
- 部署账号可运行 `docker`（通常加入 `docker` 组），日常部署不需要 root。
- 主机能访问 `ghcr.io`，不需要访问 `github.com`。

### 3.2 部署目录

部署目录（示例 `/path/to/mcp-db`）由管理员手工创建并维护，CI 不会上传任何文件：

```text
/path/to/mcp-db/
  docker-compose.yml
  deploy.sh
  run-deploy.sh
  bootstrap.sh
  keys/master.key       # bootstrap 生成，0400，uid 10001
  data/                 # 0700，uid 10001，升级时保留
```

把仓库 `deploy/` 下的四个文件复制到该目录。脚本和 compose 归管理员所有并保持只读（脚本 `0755`、compose `0644`，属主为 root 或管理员），部署账号只有读取和执行权限。不要在部署目录里存放业务配置或明文凭据。

### 3.3 初始化主密钥和目录

以 root 在该目录执行一次：

```bash
sudo bash bootstrap.sh
```

`bootstrap.sh` 幂等，并遵守以下规则：

- 需要 root；
- 首次运行且没有状态时生成 32 字节主密钥，写入 `keys/master.key`，权限 `0400`、属主 `10001`；
- `keys/` 为 `0755`，`data/` 为 `0700`、属主 `10001`；
- 已存在的主密钥只校正权限和属主，**从不覆盖**；已存在但为空或格式不对时直接失败，要求从备份恢复；
- 如果 `data/state.db` 或 `data/credentials.enc` 已存在而主密钥缺失，拒绝生成新密钥，避免旧数据永久无法解密；
- 拒绝符号链接；只对列出的路径改属主，不做递归 `chown`；
- 不打印密钥内容。

主密钥和 `data/` 必须成对备份。丢失主密钥后，已保存的连接串无法恢复。

### 3.4 compose 单元

`docker-compose.yml` 使用长语法 bind mount 和 `create_host_path: false`：`data/` 或 `keys/` 缺失时直接报错，不会静默创建空目录。`data/` 可写并跨升级保留，`keys/` 只读。两个端口只发布到 `127.0.0.1`：

- `127.0.0.1:8787`：MCP
- `127.0.0.1:8788`：管理面

不要把端口改成 `0.0.0.0`。

### 3.5 部署密钥

为 CI 生成专用密钥，把公钥写入部署账号的 `authorized_keys`，必须带 `restrict` 和固定的 forced command。路径使用绝对路径：

```text
restrict,command="/path/to/mcp-db/run-deploy.sh" ssh-ed25519 AAAA... github-actions-mcp-db-deploy
```

- 先独立核对主机指纹，再把 `DEPLOY_KNOWN_HOSTS` 写入 environment secret。
- `restrict` 关闭 PTY、端口转发、agent 转发和 X11；该密钥不能执行 shell、上传文件或转发端口。
- 该密钥只用于 forced command。SSH 隧道和日常管理使用另一把普通管理员密钥，不要复用部署密钥。
- 怀疑泄露时立即移除该公钥并轮换 `DEPLOY_SSH_KEY`。

## 4. 部署与回滚

推送到 `main` 后流水线自动执行。主机侧行为：

1. `run-deploy.sh` 用 Bash 整串锚定正则只接受这一种请求：

   ```text
   deploy ghcr.io/flandern1211/mcp-db@sha256:<64 位小写十六进制>
   ```

   其他 owner、其他仓库、标签、额外参数、前后空白、多行输入和 shell 语法全部拒绝；通过后只把校验过的镜像引用作为单个参数交给 `deploy.sh`，不使用 `eval`。

2. `deploy.sh` 再次校验摘要格式，确认 `docker-compose.yml`、`keys/master.key` 和 `data/` 存在且不是符号链接，检查主密钥 `0400`/uid `10001`、`keys/` `0755`、`data/` `0700`/uid `10001`。任何一项不满足都失败关闭。
3. 对部署目录持只读文件描述符并用 `flock` 串行化；已有部署运行时直接拒绝。
4. 先 `docker compose pull` 该摘要；拉取失败立即退出，**不触碰**当前运行容器。拉取默认超时 900 秒（`MCPDB_DEPLOY_PULL_TIMEOUT`），因为首次拉取整个镜像受出口带宽影响可能超过 5 分钟；超时同样按失败处理，不重试。
5. 再执行 `docker compose up -d --wait --wait-timeout 180 --pull never`。启动失败或健康超时直接失败，不自动重试、不自动回滚。
6. 要求端口映射恰好是上述两个回环映射，并直接请求 `http://127.0.0.1:8787/healthz` 和 `http://127.0.0.1:8788/admin/login`，两者都必须是 200。
7. 不写部署戳记，不输出业务日志。结果不确定时停止自动化，由管理员检查。

手工回滚只使用已发布的已知摘要，并先确认当前 `data/` 的状态与目标版本兼容（数据库结构/迁移方向）：

```bash
cd /path/to/mcp-db
bash deploy.sh 'ghcr.io/flandern1211/mcp-db@sha256:<已知摘要>'
```

回滚同样经过 key/data 检查、pull、健康门禁和端口检查；没有后台自动回滚。不要用 `docker compose down`、删除卷或清理数据来“重试”。

## 5. 访问

两个端口都只在主机回环地址。用普通管理员密钥建立 SSH 隧道：

```bash
ssh -N \
  -L 8787:127.0.0.1:8787 \
  -L 8788:127.0.0.1:8788 \
  <管理员账号>@<host>
```

然后使用：

- MCP：`http://127.0.0.1:8787/mcp`，带 `Authorization: Bearer <agent-token>`
- 管理台：`http://127.0.0.1:8788/admin`

首次登录的一次性令牌在主机上生成：

```bash
docker exec mcp-db mcp-db admin-login-token
```

跨主机直接访问、TLS 和反向代理不在当前范围内。

## 6. 日常运维

```bash
cd /path/to/mcp-db
docker ps --filter name=mcp-db
docker inspect --format '{{.Config.Image}}' mcp-db   # 当前运行的摘要
```

compose 文件里的 `image` 是必填变量，直接运行 `docker compose ps` 会因缺少 `MCPDB_IMAGE` 失败；查状态用上面的 `docker ps`。

- 当前镜像以 `docker inspect` / `docker compose ps` 为准，没有 `.deployed-image` 戳记。
- 需要排查容器时由管理员在主机上按需查看日志；CI 不打印业务日志。
- 更新镜像只通过 `main` 流水线或管理员手工执行已知摘要的 `deploy.sh`；不要使用 `latest` 或可移动标签。
- 端口、卷、环境变量和 compose 属于主机配置，由管理员修改后手工 `docker compose up -d`；CI 不下发配置。
- 备份 `data/` 和 `keys/master.key`，两者必须成对保存。

## 7. 已知限制

- 单机部署，不支持多实例或高可用。
- 没有 TLS 终止和反向代理，因此只支持回环访问。
- 健康门禁只证明容器启动、端口和两个 HTTP 端点可达；数据库连通性和业务调用仍需管理员人工验证。
- 主机匿名拉取要求 GHCR 包为 public；首次发布需要按 2.2 节手工切换可见性。
- 部署可能遇到 GHCR 或主机网络抖动；失败时保留现场，不自动重试。
- CI 复用 `scripts/verify.sh fast` 而不是另写一套 Rust 命令；镜像构建和容器冒烟是独立步骤。
