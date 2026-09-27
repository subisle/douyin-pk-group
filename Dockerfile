# ---------- 阶段 1：构建前端 ----------
FROM node:22-bookworm-slim AS web
WORKDIR /app
# 不用 corepack：它拉到的 pnpm 大版本随镜像漂移，pnpm 11+ 会忽略
# package.json#pnpm 里的 onlyBuiltDependencies，直接把 install 打成 ERR_PNPM_IGNORED_BUILDS。
# 显式装与 CI 一致的版本，构建结果可复现。
RUN npm install -g pnpm@10.33.0
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/
COPY apps/server/package.json apps/server/
COPY packages/shared/package.json packages/shared/
RUN pnpm install --frozen-lockfile
COPY packages/shared packages/shared
COPY apps/web apps/web
RUN pnpm --filter @pk/web build

# ---------- 阶段 2：编译 Rust 后端 ----------
FROM rust:1-bookworm AS api
WORKDIR /app
COPY rust rust
RUN cd rust && cargo build --release --bin pk-server

# ---------- 阶段 3：运行时 ----------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/*
ENV TZ=Asia/Shanghai
WORKDIR /app
COPY --from=api /app/rust/target/release/pk-server /usr/local/bin/pk-server
COPY --from=web /app/apps/web/dist /app/web
ENV WEB_DIST=/app/web \
    PORT=8787 \
    SQLITE_PATH=/app/data/app.db
VOLUME ["/app/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s \
  CMD ["/usr/local/bin/pk-server", "--health-check"]
ENTRYPOINT ["/usr/local/bin/pk-server"]
