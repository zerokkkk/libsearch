---
name: libsearch
description: 用 libsearch（本地多库文档搜索引擎，BM25+ONNX 向量+RRF）建库、索引、检索本地文档目录，并管理其嵌入模型与词典。用户要求"搜索/索引本地文档、建文档库、下载模型/词典"时使用。不适用于网络搜索或通用文本 grep。
---

# libsearch

二进制 `libsearch.exe`（Windows 另有 `daemon.exe`）；全部数据位于**程序目录（home）**：`--home` > 环境变量 `LIBSEARCH_HOME` > exe 所在目录。

## 核心流程

1. 建库：`libsearch lib add <id> <文档根目录>`
2. 索引：`libsearch reindex <id>`（增量哈希 diff；`--full` 全量）
3. 检索：`libsearch search <id> "<查询>"`（BM25+向量 RRF 融合；`--json` 出 JSON）

## 资源初始化（一次性）

- 模型：`libsearch model pull ParaphraseMLMiniLML12V2Q`（推荐）或任意 HF repo `--repo <org/name> --file <路径>`
- 词典：`libsearch dict download cc-cedict`（`--force` 重新下载）
- 换模型/词典后须 `reindex`（model_fp / dict_fp 变化检测）；用 Q 量化模型时把库 `config.json` 的 `embedding.dir` 改为 `models/ParaphraseMLMiniLML12V2Q`

## 约束（易错点）

- Windows 须设 `ORT_DYLIB_PATH` 指向具体 onnxruntime.dll 文件（或将 dll 放 exe 旁），否则检索在 dylib 加载时报错
- 词典 `format_version` 须与 lindera 版本严格匹配：main 分支用 6.2.0 预编译词典；dev 分支用本地编译的 7.0.0 词典（format 3）；不匹配时加载被拒绝
- 词典缺失时 FTS 自动回退原文（不报错）；home 根下可选用户词典 `userdict.csv` 参与索引/查询两端 FTS 分词
- daemon（`ping` / `status` / `reload` / `search --daemon`）仅 Windows；换模型/词典后调用 `reload`

## 验证

`libsearch search <id> "<语料中存在的文本>"` 应返回含 bm25 / vector / fusion 字段的排序结果；`lib list` 显示已索引文件数。
