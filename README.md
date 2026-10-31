# libsearch

本地轻量多库文档搜索引擎：BM25（lindera CC-CEDICT 分词）+ 轻量向量（ONNX CPU）+ RRF 融合。单二进制，无服务、无外部依赖。

## 需求

- Rust 工具链（rustc >= 1.89，edition 2024）
- 网络：模型（Hugging Face）与词典（GitHub releases）下载
- 仅 Windows：常驻 daemon（命名管道 IPC）；构建 / 索引 / 检索跨平台

## 安装

```bash
git clone git@github.com:zerokkkk/libsearch.git
cd libsearch
cargo build    # target\debug\libsearch.exe（Windows 同时产出 daemon.exe）
```

## 一次性初始化

```bash
libsearch model pull ParaphraseMLMiniLML12V2Q   # 嵌入模型（INT8 量化，推荐）
libsearch dict download cc-cedict               # FTS 词典（GitHub releases 预编译）
```

用 Q 量化模型时，需把各库 `config.json` 的 `embedding.dir` 改为 `models/ParaphraseMLMiniLML12V2Q`（默认值指向 `models/paraphrase-multilingual-MiniLM-L12-v2`）。

## 快速开始

```bash
libsearch lib add demo <文档根目录>   # 建库（写 config.json + index.db）
libsearch reindex demo               # 索引（增量，哈希 diff）
libsearch search demo "查询文本"      # 检索（BM25 + 向量 RRF 融合）
```

## 程序目录（home）

解析优先级：`--home <目录>` > 环境变量 `LIBSEARCH_HOME` > exe 所在目录（符号链接跟链到真实路径）。

```
<home>\
  database\libs\<lib>\   # config.json + index.db（SQLite: files / chunks / chunks_fts / vectors）
  models\                # 嵌入模型，每目录一个
  dict\                  # lindera 词典，每目录一个
  userdict.csv           # 可选用户词典（参与 FTS 分词）
```

## 环境变量

| 变量 | 用途 |
|---|---|
| `LIBSEARCH_HOME` | 程序目录 |
| `ORT_DYLIB_PATH` | 指向具体 `onnxruntime.dll` 文件（Windows 必需，或将 dll 放 exe 旁） |
| `HF_ENDPOINT` / `HF_TOKEN` | Hugging Face 镜像 / 认证 token（model pull） |

## 常用命令

| 命令 | 说明 |
|---|---|
| `lib add <id> <root> [-r] [-R] [-i <re>]... [-x <re>]...` | 建库 |
| `lib list` / `lib remove <id> [-y]` | 列出 / 删除库 |
| `reindex <lib> [--full]` | 增量 / 全量重建 |
| `model pull <preset> \| --repo <org/name> --file <路径> [--dir <名>]` | 下载嵌入模型 |
| `dict download <name> [--force] [--version <v>]` | 下载预编译词典（ipadic / ipadic-neologd / unidic / sudachidict / ko-dic / cc-cedict / jieba） |
| `search <lib> <query> [--json] [--daemon]` | 检索 |
| `ping` / `status` / `reload` | daemon 存活 / 状态 / 清缓存（Windows） |

## 分支与词典版本

| 分支 | lindera | 词典来源 |
|---|---|---|
| `main` | 6.2.0（crates.io） | `dict download`（GitHub releases，format_version 2） |
| `dev` | 7.0.0（本地 path 依赖） | 本地编译（format_version 3） |

词典 `format_version` 必须与 lindera 加载器版本严格一致（新旧均拒绝）；切换分支时 `dict\` 下需换对应词典目录。

## 故障排查

- `Failed to load ONNX Runtime dylib` → 设 `ORT_DYLIB_PATH` 指向具体 .dll 文件，或将 onnxruntime.dll 放 exe 旁
- 「向量索引过期」（model_fp 不一致）→ `reindex`（换模型 / 量化版后出现）
- 词典 format 版本不匹配报错 → 用匹配的 `--version` 重新下载（main 分支用 6.2.0）
- 词典缺失时 FTS 自动回退原文（不报错）
- 详细设计、schema 与内部实现见 `README-dev.md`（开发文档，不入仓库）
