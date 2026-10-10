# 多架构构建：所有「编译 / 安装」阶段都用 --platform=$BUILDPLATFORM 跑在构建机
# 原生架构上，再交叉编译出目标架构产物。因此 arm64 不需要 QEMU 模拟。
#
# 为什么这么改：之前 api 阶段直接 `FROM rust:1`（目标架构）+ `cargo build --release`，
# 出 arm64 时整段编译都在 QEMU 里跑，25 分钟都没编译完。改成在 amd64 构建机上
# 交叉编译 aarch64 后，arm64 与 amd64 一样是分钟级。
#
# 依赖前提：sqlx 用 tls-rustls（纯 Rust，不依赖 openssl），rusqlite bundled 用
# gcc 交叉编译 C 代码——所以只要装 gcc-aarch64-linux-gnu 就能完整交叉编译。
#
# 运行阶段不含任何 RUN：只 COPY，不执行目标架构程序，因此全程零 QEMU。

# ---------- 阶段 1：构建前端（产物与架构无关） ----------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS web
WORKDIR /app
# 不用 corepack：它拉到的 pnpm 大版本随镜像漂移，pnpm 11+ 会忽略
# package.json#pnpm 里的 onlyBuiltDependencies，直接把 install 打成 ERR_PNPM_IGNORED_BUILDS。
# 显式装与 CI 一致的版本，构建结果可复现。
RUN npm install -g pnpm@10.33.0
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/
COPY apps/douyin-web/package.json apps/douyin-web/
COPY apps/server/package.json apps/server/
COPY packages/shared/package.json packages/shared/
RUN pnpm install --frozen-lockfile
COPY packages/shared packages/shared
# 镜像里的前端 = 现役管理端 apps/douyin-web（统一后台，history 路由挂 /admin）。
# apps/web 已冻结（旧 hash 路由，见 docs/00 §7），只在 install 阶段留着对齐 pnpm-lock 的 importer，
# 不再打进镜像 —— 否则 147 / rk3318 会拿到一套过时的界面。
COPY apps/douyin-web apps/douyin-web
RUN pnpm --filter @pk/douyin-web build

# ---------- 阶段 2：交叉编译 Rust 后端 ----------
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS api
ARG TARGETPLATFORM
# gcc-aarch64-linux-gnu 只带编译器，不带目标 libc 头文件；ring 等 crate 要用
# C 编译，会 include 目标平台的 bits/libc-header-start.h，缺了会报
# "fatal error: bits/libc-header-start.h: No such file or directory"。
# 所以必须同时装 libc6-dev-arm64-cross（提供 aarch64 sysroot 头文件）。
RUN apt-get update && apt-get install -y --no-install-recommends \
      gcc-aarch64-linux-gnu \
      libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY rust rust
RUN set -eux; \
    case "$TARGETPLATFORM" in \
      linux/amd64) rust_target=x86_64-unknown-linux-gnu;  linker=gcc ;; \
      linux/arm64) rust_target=aarch64-unknown-linux-gnu; linker=aarch64-linux-gnu-gcc ;; \
      *) echo "不支持的平台: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    rustup target add "$rust_target"; \
    linker_var="CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER"; \
    export "$linker_var=$linker"; \
    cd rust; \
    cargo build --release --target "$rust_target" --bin pk-server; \
    mkdir -p /out; \
    cp "target/$rust_target/release/pk-server" /out/pk-server

# ---------- 阶段 3：CA 证书 + 时区（在构建机装好后 COPY，避免运行阶段执行目标架构程序） ----------
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

# ---------- 阶段 4：运行时（只有 COPY，无 RUN） ----------
FROM debian:bookworm-slim
ENV TZ=Asia/Shanghai
WORKDIR /app
COPY --from=base /etc/ssl/certs /etc/ssl/certs
COPY --from=base /usr/share/zoneinfo /usr/share/zoneinfo
COPY --from=api /out/pk-server /usr/local/bin/pk-server
COPY --from=web /app/apps/douyin-web/dist /app/web
ENV WEB_DIST=/app/web \
    PORT=8787 \
    SQLITE_PATH=/app/data/app.db
VOLUME ["/app/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s \
  CMD ["/usr/local/bin/pk-server", "--health-check"]
ENTRYPOINT ["/usr/local/bin/pk-server"]
