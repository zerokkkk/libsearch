# libsearch

本地轻量多库文档搜索引擎：关键词检索（BM25）+ 轻量向量检索（ONNX CPU，fastembed-rs）+ RRF 融合。
暂不含 reranker、不含 LLM，模型与数据全部本地。实现语言为 Rust（本机 rustc 1.98.1；fastembed-rs 为 Rust 生态）。
当前进度在`PROGRESS.md`

## 目录布局

所有库相关产物（配置、索引）都保存在**程序目录**下，每个库占一个目录。
库的根目录可以移动：只需修改该库 `config.json` 中的 `root`，索引不受影响——
chunk 以"相对路径 + 内容哈希"标识，不含绝对路径。

```
<program>\
  database\
    libs\
      lib-a\
        config.json    # 库配置（含 root）
        index.db       # SQLite: files / chunks / chunks_fts / vectors 四张核心表
  models\
    <模型名>\                           # 嵌入模型（ONNX + tokenizer 文件）；可有多个，各库可用不同模型
    paraphrase-multilingual-MiniLM-L12-v2\   # fp32 版；在用（config 默认）
    paraphrase-multilingual-MiniLM-L12-v2-onnx-Q\   # INT8 量化版（手工克隆自 Qdrant repo）
    ParaphraseMLMiniLML12V2Q\              # `model pull` 下载的量化版（预设名 = fastembed 变体名）
  dict\
    cc-cedict\                              # lindera CC-CEDICT 词典（预编译产物，`dict download` 下载）
  third_party\
    onnxruntime-win-x64-1.31.1\              # onnxruntime.dll（运行时动态加载）
  testdata\                                 # 测试语料（demo 库的 root）
  userdict.csv                              # 可选：用户词典（lindera 用户词典 CSV）
  src\                                      # 源码（bin\probe_onnx.rs ONNX 探针、bin\probe_scan.rs 暴力扫描探针、bin\daemon.rs daemon 入口）
```

## 代码结构

| 部分 | 源文件 |
|---|---|
| CLI 入口与命令分派（daemon 客户端：`ping`/`status`/`reload`、`search --daemon`） | `src/main.rs` |
| 库配置（schema + 默认值） | `src/config.rs` |
| 库枚举 / 添加 / 删除 | `src/libs.rs` |
| SQLite schema 与连接 | `src/db.rs` |
| 切块：breakpoints + generic-kv 模板 | `src/chunking.rs` |
| 模型管理：pooling 推导 + 模型下载（`model pull`） | `src/model.rs` |
| 词典管理：预编译词典下载（`dict download`） | `src/dict.rs` |
| 嵌入模型 tokenizer（token 计数 / 偏移）+ model_fp 计算 | `src/model_tokenizer.rs` |
| FTS 分词（lindera CC-CEDICT + 用户词典） | `src/ftstok.rs` |
| fastembed 嵌入封装 | `src/embed.rs` |
| 增量 reindex（哈希 diff、stale 检测、文件级事务） | `src/reindex.rs` |
| 检索核心（`search_core` / `SearchOutput` / 输出打印） | `src/search.rs` |
| 程序目录与资源路径解析 | `src/home.rs` |
| 命名管道 IPC（Windows，JSON Lines） | `src/ipc.rs` |
| daemon（模型/分词器进程内缓存，ping/status/reload/search） | `src/daemon.rs` |
| daemon 入口（bin） | `src/bin/daemon.rs` |
| ONNX 探针 | `src/bin/probe_onnx.rs` |
| 暴力扫描探针（性能实测） | `src/bin/probe_scan.rs` |

## 构建与运行

- 构建：`cargo build`（rustc 1.98.1，edition 2024）；产物 `target\debug\libsearch.exe`
- daemon（仅 Windows）：`cargo build` 同时产出 `target\debug\daemon.exe`；启动 `daemon.exe --home <程序目录> [--warm <lib>]`；见下文「daemon」
- 程序目录（home）解析优先级：`--home <目录>` 全局参数 > 环境变量 `LIBSEARCH_HOME` > 当前 exe 所在目录（**exe 为符号链接时先跟链到真实路径**）
- onnxruntime 动态加载：`onnxruntime.dll` 放 exe 旁，或设 `ORT_DYLIB_PATH` 指向具体文件 `third_party\onnxruntime-win-x64-1.31.1\lib\onnxruntime.dll`（指到顶层目录会加载失败）
- FTS 词典：lindera 6.2.0（crates.io 依赖）；预编译词典经 `libsearch dict download cc-cedict` 下载到 `dict\cc-cedict\`（来源 GitHub releases）；词典缺失时 FTS 自动回退原文，不报错
- 可选用户词典 `userdict.csv`（程序目录根下；lindera 用户词典 CSV，按主词典 `metadata.json` 的 `user_dictionary_schema` 解析）：存在时参与索引/查询两端 FTS 分词

## CLI

```
libsearch --home <程序目录> <cmd>
```

| 命令 | 说明 |
|---|---|
| `lib add <id> <root> [-r] [-R] [-i <re>]... [-x <re>]...` | 建库：写 `config.json` + 空 `index.db`；id 只允许字母/数字/`-`/`_`，root 须存在 |
| `lib list` | 列出全部库（id / root / 已索引文件数） |
| `lib remove <id> [-y]` | 删库（整目录含索引）；默认交互确认，`-y` 跳过 |
| `reindex <lib> [--full]` | 增量重建（哈希 diff）；`--full` 强制全量重建 |
| `model pull <preset> \| --repo <org/name> --file <路径> [--dir <名>] [--extra <路径>]...` | 下载嵌入模型到 `models\<名>` 并验证加载（见「嵌入模型」）；两种来源形式互斥 |
| `dict download <name> [--force] [--version <v>]` | 从 GitHub releases 下载预编译词典到 `dict\<name>\` 并验证可加载；name：ipadic / ipadic-neologd / unidic / sudachidict / ko-dic / cc-cedict / jieba |
| `search <lib> <query> [--json] [--daemon]` | 检索（默认 text 输出；`--json` 出 JSON；`--daemon` 走常驻 daemon） |
| `ping` | daemon 存活检查（Windows，打印 `pong`） |
| `status` | daemon 状态 JSON：home / pipe / uptime / 各库文件数 / 已缓存模型与分词器（Windows） |
| `reload` | 清空 daemon 的模型/分词器缓存（模型/词典更换后调用）（Windows） |

细节：

- `lib add`：`-x` 不指定 → 保留默认排除规则 `(^|/)(\.git|node_modules|target|\.venv|dist|build)(/|$)`；`-r`/`-R`/`-i` 全不指定 → 自动 `include_recursive=true`；写出的 `config.json` 即完整默认配置
- `config.json` 中除 `id`/`root` 外**所有字段均可省略**（serde 默认值与 `lib add` 写出的配置一致）；手改时以 `lib add` 输出为模板改值即可
- `search` 报错路径：索引为空（先 reindex）、查询为空、查询无可检索词项（全为标点/空白）、向量索引过期（model_fp 不一致，先 reindex）

## 嵌入模型

- **模型目录名 = 模型身份**：`embedding.dir` 指向 `models\<模型名>\`（相对程序目录或绝对路径），每库可用不同模型
- **pooling 不在配置中**：运行时按模型目录名查 fastembed 预设表推导（`src/model.rs` `pooling_for`）；非预设名（自定模型/手工克隆）回退 `Mean`
- **ONNX 文件发现**（`src/embed.rs` `find_onnx`）：模型目录下递归找 `.onnx`——优先标准名 `model.onnx`（树内唯一时），否则仅一个 `.onnx` 时用之，否则报错；同目录存在 `<模型文件>.data` 时自动作为 ONNX 外部数据加载（量化/大模型常见）
- **下载模型**：

```
libsearch model pull <preset>                       # fastembed 预设（变体名，大小写不敏感）
libsearch model pull --repo <org/name> --file <repo内路径> [--dir <名>] [--extra <路径>]...   # 任意 HF repo
```

  - 预设来源 = fastembed 7.5 内置 manifest（如 `ParaphraseMLMiniLML12V2` → `Xenova/paraphrase-multilingual-MiniLM-L12-v2` 的 `onnx/model.onnx`；`ParaphraseMLMiniLML12V2Q` → `Qdrant/paraphrase-multilingual-MiniLM-L12-v2-onnx-Q` 的 `model_optimized.onnx`）
  - 下载内容：模型文件 + manifest `additional_files` + 4 个 tokenizer 文件（repo 根）+ `--extra` 文件；`<模型文件>.data` 尽力下载（repo 无则跳过）
  - 下载后**验证加载**（实际建 ORT session，外部数据缺失会点名缺失文件）；拒绝覆盖已存在的非空目录
  - 尊重 `HF_ENDPOINT`（可指镜像）与 `HF_TOKEN`；下载缓存 `<home>\.hf-cache\`
- **model_fp**（chunks/vectors 表的 stale 检测键）：`目录名|pooling|sha256(tokenizer.json)`；任一要素变化（换模型、换量化版）→ reindex 检测 stale 并重编码

## 库（library）

- 一个库对应一个目录 `database\libs\<id>\`，**目录名即库 id**（自定义字符串，唯一）
- `config.json` 是库的唯一配置，**其中的 `root` 是根目录的唯一来源**，root 可变
- 枚举库 = 扫描 `database\libs\` 的子目录

### 库配置 schema

```json
{
  "id": "lib-a",
  "root": "D:\\docs\\a",
  "chunking": {
    "max_tokens": 512,
    "overlap_ratio": 0.15,
    "by_type": {
      "markdown": { "strategy": "breakpoints" },
      "yaml":     { "strategy": "template", "template": "generic-kv" },
      "json":     { "strategy": "template", "template": "generic-kv" }
    }
  },
  "retrieval": {
    "methods": ["bm25", "vector"],
    "fusion": { "method": "rrf", "k": 60, "weights": [1.0, 1.0] },
    "bm25":   { "top_k": 20 },
    "vector": { "top_k": 20 },
    "top_k": 10,
    "min_score": 0.0,
    "dedup_per_file": false
  },
  "dir_filter": {
    "include_root": true,
    "include_recursive": true,
    "include": [ "\\.txt$" ],
    "exclude": [ "(^|/)(\\.git|node_modules|target|\\.venv|dist|build)(/|$)" ]
  },
  "embedding": {
    "dir": "models/paraphrase-multilingual-MiniLM-L12-v2"
  },
  "fts": {
    "dict_dir": "dict/cc-cedict"
  }
}
```

字段说明：

- `chunking`：只在索引时使用，**查询时不可指定**；`by_type` 按文件类型分派切块策略，**未列出的长文本类型默认 `breakpoints`**
- `max_tokens` 应 ≤ 嵌入模型窗口（512），保证整块嵌入不截断
- 受支持文件类型（v1）：`.txt`/`.md`（长文）、`.yaml`/`.yml`/`.json`（短片段）
- `dir_filter`（所有 pattern 均为**正则**，匹配文件的相对路径）：
  - `include_root`：包含仅根目录下的条目
  - `include_recursive`：包含文件夹及子文件夹（递归）
  - `include`：更一般的 include 正则列表；**为空 = 收遍历范围内的全部文件**
  - `exclude`：排除正则列表
  - 配置库（`lib add`）时用三个参数指定 include：`-root` = 仅根目录下的条目；`-recursive` = 文件夹及子文件夹；`-include "pattern"` = 更一般的正则
- 其余检索参数（各路 top_k、RRF 的 $k$ 与 $w_l$、输出 top_k、min_score）都在 `retrieval` 下；**各字段均可省略**（serde 默认值与 `lib add` 写出的配置一致），仅 `id`/`root` 必填
- `embedding.dir`：嵌入模型目录（相对程序目录或绝对路径），每库可用不同模型
- `fts.dict_dir`：lindera 词典目录（编译产物目录）；`null` = 无词典（FTS 回退原文），每库可用不同词典

### index.db schema

SQLite（WAL），每库四张核心表：

| 表 | 键 | 列 |
|---|---|---|
| `files` | `relpath` | `content_hash`（sha256）、`size`、`indexed_at`；reindex diff 依据 |
| `chunks` | `chunk_id` | `file`、`kind`、`strategy`、`seq`、`start_off`/`end_off`（半开区间）、`start_line`/`end_line`、`content_hash`、`model_fp`、`dict_fp`、`content`（template 存渲染文本） |
| `chunks_fts` | FTS5 虚表 | `chunk_id`（UNINDEXED）、`content`（预分词文本，unicode61） |
| `vectors` | `chunk_id` | `model_fp`、`dims`（384）、`embedding`（f32 little-endian BLOB，模型原始输出） |

## 切块

不同文本类型用不同方式：

### `breakpoints`（长文，md/txt）

照搬 qmd 的方式（参考 [store.ts](D:/Code/AGENT/query/qmd/qmd-main/src/store.ts)）：

- 每块 512 token（= 嵌入模型窗口，保证整块嵌入不截断），15% overlap（76 token）
- 断点：标题、空行；**代码围栏内不切断**
- 滑动窗口在目标终点附近找最佳切点（窗口 = 0.7 × 块大小）
- token 计数：用嵌入模型的 subword tokenizer（`tokenizers` crate，HF tokenizer.json，加载后已清 truncation），不做字符估算

### `template`（yaml/json 短片段）

- 内置通用模板 `generic-kv`：把结构化内容拍平成"路径缩进的 key: value"文本后入库
  （模板按行对齐原文件，每行可回溯到源码行号）
- 短文件整体一块；超长按模板分组再切
- 渲染发生在**索引时**：FTS 与向量都基于渲染文本；改模板需重索引

未列出的长文本类型（如 `.txt`）默认使用 `breakpoints`。

### Chunk 元数据（文件内位置）

每个 chunk 记录必须标明它是文件的哪到哪：

| 字段 | 含义 |
|---|---|
| `chunk_id` | 库内唯一：`<lib_id>\|<relpath>#<seq>`（seq 为该文件内序号） |
| `file` | 相对 root 的路径 |
| `kind` | `markdown` / `txt` / `yaml` / `json` |
| `strategy` | `breakpoints` / `template` |
| `start`, `end` | 字符偏移，半开区间 $[start, end)$（指向原始文件） |
| `start_line`, `end_line` | 行号，1-based 闭区间（指向原始文件；template 块映射回源码行） |
| `content_hash` | chunk 文本 sha256 |
| `model_fp` | 嵌入模型指纹（模型+版本），模型变化时整库向量失效 |
| `dict_fp` | 词典指纹（词典目录 + 用户词典），变化时仅触发 FTS 重分词（向量保留） |

## 检索管线（模式 A：无持续进程）

每条查询一个进程，全部状态在磁盘：

```
query
 ├→ BM25        SQLite FTS5（lindera 词级预分词文本），按 chunk 打分
 └→ vector      fastembed-rs (ONNX CPU) 编码 query → 与库内 chunk 向量暴力余弦
        ↓
   RRF 融合
        ↓
   top_k 输出: 相对路径 + 行范围 + 片段 + 各路分数 + 融合分（text 或 JSON）
```

RRF 融合公式: $score(d) = \sum_l w_l / (k + rank_l(d))$

- 默认 $k=60$、各路等权、输出 top_k=10
- 输出单元 = chunk；`dedup_per_file=true` 时每文件只保留最高分 chunk
- v1 无 reranker、无 LLM 查询扩展
- 向量检索 v1 用暴力扫描（10 万 chunk × 384 维 ≈ 每查询 4×10⁷ 次乘加；本机单线程实测：release ~51ms、debug ~518ms，`probe_scan` 探针）；后续可加 usearch HNSW
- 嵌入模型：`paraphrase-multilingual-MiniLM-L12-v2`（384d，中英混合语料）
- BM25 查询构造：lindera 词项 OR 连接；纯 ASCII 词加 * 前缀；BM25 分数归一化 $s' = |s|/(1+|s|)$
- FTS5 中文：lindera CC-CEDICT 词级分词（`dict\cc-cedict`；词典缺失时回退原文）

嵌入模型实现用 fastembed（crate 名 `fastembed`，v7）加载本地模型文件：`try_new_from_user_defined`
（本地 ONNX + 4 个 tokenizer 文件；pooling 按模型目录名查预设表推导，见「嵌入模型」节）。
Windows 上启用 `ort-load-dynamic` feature，运行时加载官方动态库 `onnxruntime.dll`
（放 exe 旁或设 `ORT_DYLIB_PATH`）；pyke 预编译静态 onnxruntime（/MT CRT）与 Rust 默认 /MD 链接冲突（LNK2005），不可用。

### 检索输出格式

- text：首行 `query: ...   lib: ...   （N 条结果）`；每条两行：`rank. <relpath>:<start_line>-<end_line>   fusion=...   [bm25=... rN, vector=... rN]` + 片段（120 字符截断）
- JSON（`--json`）：`{lib, query, methods, count, results[]}`；每条含 `rank, chunk_id, file, kind, start_line, end_line, snippet, fusion`，命中路另有 `bm25`/`bm25_rank`/`vector`/`vector_rank`（未命中的路省略）
- 向量路：查询时算余弦（query 向量与 chunk 向量均运行时归一化）；`model_fp` 不一致视为向量索引过期，须先 reindex

## 索引与增量（只用哈希 diff）

1. 按 `dir_filter` 遍历 root，对每个受支持文件计算**内容哈希**
2. 与 `files` 表（relpath → hash）比对：
   - 新增/变化文件 → 重新切块 + 向量编码，写入前**先删除该文件旧 chunk/fts/vector 集合**（文件内切块整体替换）
   - 被删文件 → 删除其全部 chunk
   - 模型指纹变化或向量缺失的文件 → 重新切块 + 重编码（自动检测，无需手工干预）
   - 词典指纹变化（或旧库 NULL）→ 仅重分词 FTS 文本（向量保留，不重编码）
   - 未变文件 → 不动
3. 根目录移动不触发任何重索引（一切以 relpath + hash 为键），改 `config.json` 的 `root` 即可
4. `reindex` 命令执行上述流程；提供 `--full` 强制全量重建

## daemon（模式 B：常驻进程，仅 Windows）

- engine 抽成库（`src/lib.rs`），CLI 与 daemon 共用 `search_core` / reindex 等核心
- daemon = 单线程同步 accept 循环（命名管道 `.single()` 独占实例）；进程内缓存 Embedder（键=model_dir）
  与 FTS 分词器（键=(dict_dir, user_dict)），首次使用懒加载；`--warm <lib>` 预热
- IPC：管道名 `\\.\pipe\libsearch-<sha256(canonicalize(home)) 前 8 位>`；JSON Lines（一行一个 JSON）；
  请求 `{"op":"ping|status|reload|search",...}`，响应 `{"ok":bool,"payload":{...},"error":"..."}`；
  客户端 `WaitNamedPipeW` 预检（2s），daemon 未运行快速报错
- `search --daemon` 显式开启（默认本地行为不变）；`ping`/`status`/`reload` 做存活检查 / 状态 / 清缓存
- reindex 不走 daemon（留 CLI）：单线程串行，长 reindex 会阻塞所有查询
- 非 Windows：`named_pipe` 是 Windows-only crate → `ipc`/`daemon` 与相关 CLI 经 `#[cfg(windows)]` 门控，
  核心（切块/reindex/检索）仍可跨平台编译
- 实测（demo 库）：warm 首查 ~0.87s vs 本地 ~6.7s（模型加载 ~5.5s 占大头）；text/JSON 逐字节一致

## 参考实现

- 切块（breakpoints）：[qmd store.ts](D:/Code/AGENT/query/qmd/qmd-main/src/store.ts)
- 融合/RRF 变体：[WeKnora knowledgebase_search_fusion.go](D:/Code/AGENT/query/WeKnora/WeKnora-main/internal/application/service/knowledgebase_search_fusion.go)、[hindsight fusion.py](D:/Code/AGENT/query/hindsight/hindsight-main/hindsight-api-slim/hindsight_api/engine/search/fusion.py)
- 重排设计参考（v1 不实现）：[WeKnora rerank.go](D:/Code/AGENT/query/WeKnora/WeKnora-main/internal/reranking/rerank.go)

## 项目计划

- [x] **P0 骨架**：cargo 项目 + clap CLI（`lib add/list/remove`、`reindex`、`search` 占位）；
      枚举 `database\libs\` 子目录加载各库 `config.json`；SQLite schema（`files` / `chunks` / `chunks_fts` / `vectors`）
- [x] **P1 切块 + 索引**：breakpoints 切块器；generic-kv 模板渲染；
      增量 reindex（哈希 diff）；chunk 元数据落库（含 start/end、行号）
- [x] **P2 检索**：fastembed-rs ONNX 编码（query + chunk）；FTS5 BM25；暴力向量余弦；
      RRF 融合；`search` 输出 text/JSON（含文件位置元数据）
- [x] **P3 验证**：以 `query\` 下真实项目（如 fastembed、graphify）为语料做端到端测试，
      检查增量行为（改文件/删文件/移动 root）
- [x] **P4（v2）daemon 模式**：engine 库化 + 命名管道服务 + 模型/索引预加载
