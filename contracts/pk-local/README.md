# PK Local 契约

`openapi.yaml` 定义 `pk-server`（Rust）对外的最小稳定接口；`schemas/robot-command.schema.json`
定义机器人入口允许出现的指令。机器人命令路由必须保持确定性，不允许调用模型服务。

> 历史说明：本契约最初同时覆盖 Go 过渡服务与 Rust `pk-server`；后端已统一为 Rust
> 单技术栈（见 `docs/04-统一技术栈决策.md`），Go 服务退役后契约只对 `pk-server` 生效。

每个接口需在 `description` 中注明「已实现 / 计划中」，不得把占位接口写成可用功能。
API 新增字段默认向后兼容；删除字段或改语义必须提升 OpenAPI `version`。
