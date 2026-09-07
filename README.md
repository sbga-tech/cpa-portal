# CPA Portal

CPA Portal 为 CPA 增加基于 GitHub 登录的多用户自助服务。
它为每位用户创建并保存一个加密的 CPA API Key，并提供登录页、用户面板和只读本地排名页。
模型请求、路由和管理功能仍由 CPA 处理；CPA Portal 不代理模型请求。

## 功能

- GitHub OAuth 登录与会话管理
- 按 GitHub 用户绑定 CPA API Key
- 通过 CPA Management API 创建 API Key
- SQLite 持久化用户、API Key 和 OAuth 状态
- ChaCha20-Poly1305 加密存储 API Key
- 同源登录 CPA Usage Keeper
- GitHub 用户可查看 CPA Usage Keeper 本地排名
- 为内部 Bot 提供 Bearer 认证的只读 Admin API

## 路由

CPA Portal 注册以下路由：

- `GET /`：登录页或用户面板
- `GET /auth/github/start`：开始 GitHub OAuth 登录
- `GET /auth/github/callback`：处理 GitHub OAuth 回调
- `POST /logout`：退出当前会话
- `GET /ranking`：查看只读本地排名
- `GET /api/admin/v1/users`：查询 Portal 用户资料
- `GET /api/admin/v1/ranking`：查询 Portal 用户排名
- `GET /api/admin/v1/quota`：查询账号索引和 Keeper 原始额度缓存
- `GET /api/admin/v1/quota/reset-credits/{auth_index}`：查询单个 Codex 账号的可用重置次数和过期明细

`/ranking` 和 `/ranking/*` 应转发到 CPA Portal；`/usage` 和 `/usage/*` 应转发到 CPA Usage Keeper，其余路径应转发到 CPA。

## 配置

复制 `config.example.toml`，或创建 `cpa-portal.toml`：

```toml
[server]
listen = "0.0.0.0:8080"
public_base_url = "https://ai.example.com"
site_name = "CPA Portal"

[server.session]
cookie_name = "cpa_portal"
secure = true
ttl_seconds = 2592000

[github]
client_id = "YOUR_GITHUB_CLIENT_ID"
client_secret = "YOUR_GITHUB_CLIENT_SECRET"

[cpa]
public_base_url = "https://ai.example.com"
internal_base_url = "http://cpa:8317"
management_key = "YOUR_CPA_MANAGEMENT_KEY"

[keeper]
internal_base_url = "http://cpa-usage-keeper:8080/usage"
login_password = ""

[admin_api]
token = "YOUR_RANDOM_ADMIN_API_TOKEN"

[database]
url = "sqlite://./data/cpa-portal.db"

[security]
api_key_encryption_key = "YOUR_BASE64_32_BYTE_KEY"
api_key_prefix = "sk-ghu-"
```

`security.api_key_encryption_key` 必须是 32 字节随机值的 Base64 或 Base64URL 编码。
丢失该值后，数据库中已加密的 API Key 无法恢复。

```bash
openssl rand -base64 32
```

Admin API token 应至少包含 32 字节随机数据，并与 CPA management key、Keeper 登录密码分开。可使用同一命令生成。

默认加载可选的 `config.*`。
设置 `CPA_PORTAL_CONFIG` 可指定必需的配置文件，环境变量 `CPA_PORTAL__...` 可覆盖配置项，例如：

```text
CPA_PORTAL__KEEPER__LOGIN_PASSWORD=YOUR_KEEPER_LOGIN_PASSWORD
CPA_PORTAL__ADMIN_API__TOKEN=YOUR_RANDOM_ADMIN_API_TOKEN
CPA_PORTAL_CONFIG=/app/config.toml
CPA_PORTAL__SERVER__PUBLIC_BASE_URL=https://ai.example.com
CPA_PORTAL__CPA__INTERNAL_BASE_URL=http://cpa:8317
```

## GitHub OAuth App

在 GitHub 创建 OAuth App，并设置：

```text
Homepage URL: https://ai.example.com
Authorization callback URL: https://ai.example.com/auth/github/callback
```

## Docker

镜像通过当前 GitHub 仓库名发布到 GHCR：

```yaml
services:
  cpa-portal:
    image: ghcr.io/sbga-tech/cpa-portal:latest
    restart: unless-stopped
    expose:
      - "8080"
    environment:
      CPA_PORTAL_CONFIG: /app/config.toml
      CPA_PORTAL__KEEPER__LOGIN_PASSWORD: ${KEEPER_LOGIN_PASSWORD:?set KEEPER_LOGIN_PASSWORD}
      CPA_PORTAL__ADMIN_API__TOKEN: ${PORTAL_ADMIN_API_TOKEN:?set PORTAL_ADMIN_API_TOKEN}
    volumes:
      - ./cpa-portal.toml:/app/config.toml:ro
      - ./cpa-portal-data:/app/data
```

Admin API 设计为 Docker 内部接口，不应加入公开反向代理规则。调用方使用：

```http
Authorization: Bearer <PORTAL_ADMIN_API_TOKEN>
```

所有 Admin API 响应均设置 `Cache-Control: no-store`。`/quota` 只读取 Keeper cache，不会触发官方额度刷新；`keeper.version`、`keeper.auto_refresh` 和 `keeper.quota_cache` 作为不透明 JSON 原样返回，由调用方按 Keeper 版本解析。

## 反向代理

CPA Portal、CPA Usage Keeper 和 CPA 应位于同一站点下。
以下 Caddy 配置让 Portal 处理登录和排名路由，让 Keeper 处理用量页面，并将其他请求交给 CPA：

```caddyfile
ai.example.com {
	@portal path / /auth/* /logout /ranking /ranking/*
	handle @portal {
		reverse_proxy cpa-portal:8080
	}

	@usage path /usage /usage/*
	handle @usage {
		reverse_proxy cpa-usage-keeper:8080
	}

	handle {
		reverse_proxy cpa:8317
	}
}
```

用户在面板点击 **Query usages** 后，浏览器会向
`/usage/api/v1/auth/api-key-login` 发送同源 `POST` 请求。
登录成功后，浏览器跳转到 `/usage/`；API Key 不会出现在 URL 中。

## 相关链接

- CPA Portal 镜像：<https://github.com/sbga-tech/cpa-portal/pkgs/container/cpa-portal>
- GitHub OAuth App 文档：<https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/creating-an-oauth-app>
- Caddy `reverse_proxy`：<https://caddyserver.com/docs/caddyfile/directives/reverse_proxy>
