# douyin-server：RK3318 数据管理 / 采集 / 机器人服务（Go）
#
# 与根目录 Dockerfile（Rust pk-server）同一套多架构约定：
#   - TARGETPLATFORM=linux/amd64 / linux/arm64
#   - 编译阶段在构建机原生架构上做交叉编译，运行阶段纯 COPY 零 QEMU
#   - migration SQL 用 go:embed 编进二进制，运行不需要源码目录
#
# 本地/CI 用法：
#   docker build --platform linux/arm64 -f server-go/Dockerfile -t douyin-server:arm64 .
#   docker build --platform linux/amd64 -f server-go/Dockerfile -t douyin-server:amd64 .

# ---------- 阶段 1：交叉编译 ----------
FROM --platform=$BUILDPLATFORM golang:1.27-bookworm AS build
ARG TARGETOS TARGETARCH
WORKDIR /src
# 先拷依赖清单，利用层缓存
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN set -eux; \
    CGO_ENABLED=0 \
    GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags="-s -w" -o /out/douyin-server ./cmd/douyin-server

# ---------- 阶段 2：运行时（纯 COPY，零 QEMU） ----------
FROM --platform=$TARGETPLATFORM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata curl
ENV TZ=Asia/Shanghai
WORKDIR /app
COPY --from=build /out/douyin-server /usr/local/bin/douyin-server
# 机器人平台的状态目录；运行时挂 volume
RUN mkdir -p /var/lib/douyin
VOLUME ["/var/lib/douyin"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s \
  CMD ["curl", "-fsS", "http://127.0.0.1:8080/dy/api/health"]
ENTRYPOINT ["/usr/local/bin/douyin-server"]
