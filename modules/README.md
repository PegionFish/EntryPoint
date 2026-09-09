# modules/ — 模型适配层（外部导入，不入主线）

本目录**不随主线仓库分发**（见根 `.gitignore`：`modules/*` 全部忽略）。
主线仓库只携带调度引擎与内置节点；所有模型适配层以独立插件 repo 为源：

- 仓库位置：`/home/bob/EntryPoint_Models/EntryPoint_<模型>/`（本机）
- GitHub：`PegionFish/EntryPoint_<模型>`（发布面）
- 对应关系与上游血缘：见 `docs/MODEL_REPOS.md`

## 首次部署 / fresh clone 后导入

```bash
./scripts/import-modules.sh          # 按 sync 脚本映射表导入全部缺失模块
./scripts/import-modules.sh waifu2x-unlimited   # 只导入指定模块
```

## 开发与回写

适配层在本目录内开发（daemon 只扫描 `modules/`），完成后经反向同步
更新插件 repo（保持 GitHub 发布面最新，**不要**把适配层提交进主线）：

```bash
./scripts/sync-model-repos.sh --only <module_id>
```
