# CPA Portal

CPA Portal 为 CPA 提供基于 GitHub 登录的多用户自助服务：用户登录后可创建或复用自己的 CPA API Key，并查看用量排名。模型请求和路由仍由 CPA 负责，Portal 不代理模型请求。

## 功能

- GitHub OAuth 登录与会话管理
- CPA API Key 创建、绑定和加密存储
- CPA Usage Keeper 同源登录与本地排名
- 面向内部 Bot 的 Bearer 认证 Admin API

## 路由

用户侧：

- `GET /`：登录页或用户面板
- `GET /auth/github/start`：开始 GitHub OAuth
- `GET /auth/github/callback`：OAuth 回调
- `POST /logout`：退出登录
- `GET /ranking`：本地用量排名

内部 Admin API：

- `GET /api/admin/v1/users`
- `GET /api/admin/v1/ranking?period=<period>&metric=<metric>`
- `GET /api/admin/v1/quota`
- `GET /api/admin/v1/quota/history/{auth_index}?window_role=primary|secondary`
- `GET /api/admin/v1/quota/routing`
- `GET /api/admin/v1/quota/reset-credits/{auth_index}`

Admin API 只供内部调用，使用：

```http
Authorization: Bearer <PORTAL_ADMIN_API_TOKEN>
```

不要将 Admin API 加入公开反向代理规则。所有 Admin API 响应均设置 `Cache-Control: no-store`。

## 配置

从示例开始：

```bash
cp config.example.toml cpa-portal.toml
```

配置文件包含 `[server]`、`[github]`、`[cpa]`、`[keeper]`、`[admin_api]`、`[database]` 和 `[security]` 各部分。可通过 `CPA_PORTAL_CONFIG` 指定配置文件，并使用 `CPA_PORTAL__...` 环境变量覆盖配置项，例如：

```text
CPA_PORTAL_CONFIG=/app/config.toml
CPA_PORTAL__ADMIN_API__TOKEN=YOUR_RANDOM_ADMIN_API_TOKEN
CPA_PORTAL__KEEPER__LOGIN_PASSWORD=YOUR_KEEPER_LOGIN_PASSWORD
```

`security.api_key_encryption_key` 必须是稳定的 32 字节随机值的 Base64 或 Base64URL 编码。丢失或更换它会导致已存储的 API Key 无法恢复。可使用：

```bash
openssl rand -base64 32
```

GitHub OAuth App 的配置：

```text
Homepage URL: https://ai.example.com
Authorization callback URL: https://ai.example.com/auth/github/callback
```

## Docker

镜像发布到 GHCR：

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

数据库和加密数据位于 `/app/data`，需要持久化。完整配置项见 `config.example.toml`。

## 反向代理

Portal、Keeper 和 CPA 通常位于同一站点：

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

## 相关链接

- [CPA Portal 镜像](https://github.com/sbga-tech/cpa-portal/pkgs/container/cpa-portal)
- [GitHub OAuth App 文档](https://docs.github.com/en/apps/oauth-apps/creating-an-oauth-app)
- [Caddy reverse_proxy](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)
