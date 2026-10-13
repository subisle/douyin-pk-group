#!/usr/bin/env bash
# 3328 磁盘/内存 实时守卫：阈值告警 + 可选自动清理 cargo 残留
# 部署在 rk3328：/opt/pk-dev/guard.sh，systemd timer 每 5 分钟跑一次
# 状态写 /run/pk-dev-guard.status（内存模式，重启即清）；告警走 logger（journal 可查）
set -u

DISK_WARN=80    # 根分区使用率 % 告警线
DISK_CRIT=90    # 强制清理线
MEM_WARN_MB=300 # 可用内存低于此值告警
MEM_CRIT_MB=150 # 可用内存低于此值强清
STATUS=/run/pk-dev-guard.status

disk_pct=$(df -P / | awk 'NR==2{gsub("%","");print $5}')
mem_avail=$(free -m | awk 'NR==2{print $7}')
ts=$(date '+%F %T')

cleaned="no"
if (( disk_pct >= DISK_CRIT || mem_avail <= MEM_CRIT_MB )); then
  # 顶住 3328 小盘：按大小清旧编译产物/日志（只动明确安全的目标）
  find /root/.cargo -type f -name "*.crate" -mtime +14 -delete 2>/dev/null
  find /tmp -type f -size +50M -mtime +3 -delete 2>/dev/null
  journalctl --vacuum-size=32M >/dev/null 2>&1
  cleaned="yes"
  disk_pct=$(df -P / | awk 'NR==2{gsub("%","");print $5}')
  mem_avail=$(free -m | awk 'NR==2{print $7}')
fi

if (( disk_pct >= DISK_WARN || mem_avail <= MEM_WARN_MB )); then
  level=$([[ $disk_pct -ge $DISK_CRIT || $mem_avail -le $MEM_CRIT_MB ]] && echo CRIT || echo WARN)
  msg="[$level] disk=${disk_pct}% mem_avail=${mem_avail}MB cleaned=$cleaned"
  logger -t pk-dev-guard "$msg"
  echo "$ts $msg" >> "$STATUS"
  tail -n 50 "$STATUS" > "$STATUS.tmp" && mv "$STATUS.tmp" "$STATUS"
else
  echo "$ts OK disk=${disk_pct}% mem_avail=${mem_avail}MB" > "$STATUS"
fi
