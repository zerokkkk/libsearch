# libsearch 进度

## 已完成

- **P0 骨架**（前次会话）：cargo + clap CLI（`lib add/list/remove`、`reindex`、`search` 占位）、
  库目录与 config.json、SQLite schema（`files`/`chunks`/`chunks_fts`/`vectors`，WAL）。
- **P1 切块 + 索引**（本次完成并验证）：
  - `src/chunking.rs`：breakpoints 切块器（qmd 算法：900 token、15% overlap、标题/空行断点、
    代码围栏内不切断、窗口 0.7×块大小的衰减打分）；`generic-kv` 模板渲染（yaml/json 拍平为
    完整路径 `key: value` 文本，行号回溯源码）
  - `src/reindex.rs`：哈希 diff 增量 reindex（文件级事务整体替换旧 chunk；`--full` 全量重建）
  - `src/model_tokenizer.rs`：嵌入模型 subword tokenizer（HF `tokenizers` crate；
    **加载后清除 tokenizer.json 自带的 truncation**，否则长文本被静默截断到 128 token）
  - `src/ftstok.rs`：可选 CC-CEDICT 预分词——`dict/cc-cedict/` 存在时
    FTS 文本用 lindera 分词结果，否则存原文（词典已从源码编译落地，策略 B 生效）
  - chunk 元数据落库：`kind`/`strategy`/`seq`/字符偏移 `[start_off, end_off)`/
    `start_line`/`end_line`/`content_hash`/`model_fp`
- **验证**（小语料 `testdata`，4 文件）：
  - 单元测试 4/4 通过（breakpoints 小/长文件、yaml/json 渲染）
  - 全量 reindex：4 文件 4 chunks，元数据核对正确
  - 增量：改文件 → 变更 1；加文件 → 新增 1；删文件 → 删除 1；未变文件不动
  - root 移动（改 config.json 的 root）→ 0 重切块
  - `--full` → 4 文件全部重切
- **P2 预备**（本次）：
  - FTS 策略定为 **B**（lindera CC-CEDICT 词级分词）：建一次性 crate（`lindera-cc-cedict` path 依赖）并设
    `LINDERA_BUILD_DICTIONARY_CACHE_DIR`，build 脚本自动从 lindera.dev 抓取 CC-CEDICT-MeCab 并编译词典
    → `dict/cc-cedict/`（dict.words 16MB + trie/vals/索引）；`main.rs` 词典路径改为目录 + `is_dir()`
  - 嵌入实现定案：fastembed（crate 名 `fastembed` v7）`try_new_from_user_defined` 加载本地
    ONNX + 4 个 tokenizer 文件，Mean pooling（与模型 `1_Pooling` 配置一致）；默认 feature
    `ort-download-binaries` 下载的 pyke 静态 onnxruntime（/MT CRT）与 Rust /MD 链接冲突（LNK2005）
    → 改 `ort-load-dynamic` + 官方动态库 onnxruntime.dll 1.31.1（放 `third_party/`，运行时经 `ORT_DYLIB_PATH` 定位）
  - 探针 `src/bin/probe_onnx.rs` 验证：384 维、输出 L2 归一、512-token 截断生效
    （1360 字符 → 512 tokens）、ONNX 序列轴 ≥512（与 `config.json` 的 `max_position_embeddings: 512` 一致）
  - 编码规范：`pretest-before-scale` 3→4、`progress-single-line` 0→1，新增
    `hf-tokenizer-truncation`、`rust-regex-no-lookaround`
- **P2 检索**（本次完成并验证）：
  - `src/embed.rs`：fastembed 封装（`Embedder`，Mean pooling，model_fp 与 tokenizer 一致）
  - `src/reindex.rs` 写 `vectors`：文件级事务含 chunks/fts/vectors 三表删除+插入；**stale 检测**
    （model_fp 不一致或向量缺失 → 自动重切块+重编码，无需手工干预）；embedder 懒加载（无工作不加载模型）
  - `src/search.rs`：BM25（FTS5，lindera 词项 OR 连接、ASCII 词 `*` 前缀、分数归一化 $s' = |s|/(1+|s|)$）
    + 暴力余弦（model_fp 校验，384 维 f32 LE BLOB）+ RRF 融合；text/JSON 输出（rank/文件/行范围/片段/各路分数）
  - 决策落地：chunk `max_tokens` 900→512（默认与 demo 库）；demo 库全量重建
  - **验证**：单元测试 4/4；demo 全量重建 → 4 文件 4 chunks 4 vectors（384d，fp 一致，无孤儿/缺失）；
    检索 `embedding`/`hello world`/`嵌入模型`（CJK 零 BM25 命中走纯向量）结果正确，RRF 分核对（2/61=0.032787）；
    JSON 输出正常；空词项报错路径；增量 no-op（未变 4、不加载模型）与改文件（变更 1）正确
  - 运行环境已验证（免重查）：`ORT_DYLIB_PATH` 须指向 `third_party\onnxruntime-win-x64-1.31.1\lib\onnxruntime.dll`
    （dll 在 `lib\` 子目录，指到顶层目录加载失败）；CLI 运行：`target\debug\libsearch.exe --home <程序目录> <cmd>`

- **P3 验证**（本次完成）：
  - 真实语料库：`libfastembed`（17 文件 / 58 chunks）、`libgraphify`（395 文件 / 5591 chunks，全量 reindex 17.3 min）；
    `libp3inc` 为增量测试库（操作 fastembed 副本 `P3test\copy1/2`，不动原语料）
  - DB 完整性核对（脚本 `P3test\check_db.py`，4 库含 demo）：files/chunks/chunks_fts/vectors 数量一致、无孤儿；
    model_fp 一致（384 维、1536B BLOB）；seq 每文件连续；偏移/行号健全
  - 增量行为（libp3inc）：
    - root 移动（config.json `root` 改到同内容副本 copy2）→ 0 重切块（17 未变）
    - 改文件 → 变更 1；加文件 → 新增 1（chunks 58→59）；删文件 → 删除 1（chunks 59→58）；未变文件不动
  - 检索验证：英文查询双路命中（BM25+vector，RRF 把双路命中顶到前，如 2/64=0.031250）；
    中文查询命中 zh-CN README 的 BM25（lindera 分词生效）；英文语料纯向量（未命中路省略）；`--json` 正常
  - `dedup_per_file=true` 抽查（libfastembed：10→5 条、每文件 1 条；跑完已还原配置）
  - 错误路径：空查询 → `查询文本为空`；空索引 → `库 libempty 索引为空，请先 reindex`
  - 端到端检索耗时 ~6s/查询（模型加载 ~5.5s 占大头，5591 chunks 暴力扫描占比小）；
    "10 万 chunk <20ms" 未直接实测（见决策 7）

- **模型/词库自由指定**（本次，A+B+C）：
  - A：`config.json` 加 `embedding.dir` / `fts.dict_dir`（默认=原固定路径；相对路径以程序目录为基准，绝对路径亦可；
    `lib add` 写出完整配置；旧配置自动补默认）；每库可用不同模型/词典
  - B：`chunks.dict_fp`（词典目录 + 用户词典指纹；`db.rs` 对旧库自动迁移加列）；词典变化仅重分词 FTS
    （`replace_file_fts`，向量保留、不加载模型）；旧库 NULL 自动回填
  - 修复上次中断的损坏：`db.rs` `migrate` 缺 `}`、`reindex.rs` println 参数错位、`replace_file_fts` Statement 借用（commit 前须 drop）
  - 验证：单测 9/9；demo 回填（fts-only 3）→ 稳态 4 未变 → 改词典 fts-only 4 → 还原 fts-only 4（全程未加载模型）；
    `lib add` 写出新字段；无效模型目录报错；`dict_dir: null` → FTS 原文 + dict_fp="none"；检索无回归
  - C：改用 lindera 内置 `load_user_dictionary_for`——`load_dictionary` 已把词典目录 `metadata.json` 解析进 `Dictionary.metadata`，用户词典 CSV 按主词典自身 `user_dictionary_schema` 解析（CC-CEDICT：surface/part_of_speech/pinyin；ipadic/unidic：surface/part_of_speech/reading；非跨词典通用标准）；移除对 lindera-cc-cedict 目录的 `include_str!` 编译期依赖
- **P4 daemon 模式**（本次完成并验证）：
  - D1-A：单 crate 加 `src/lib.rs` 暴露 engine 模块；`src/bin/daemon.rs` 第二 bin
  - D2-A：Windows 命名管道 `\\.\pipe\libsearch-<sha256(canonicalize(home)) 前 8 位>`；JSON Lines 帧；
    `named_pipe` 0.4.1 经查证是 **Windows-only** crate（纯 winapi，无 UDS 回退——早前"跨平台落 UDS"的判断有误）
    → 移入 `[target.'cfg(windows)'.dependencies]`；`ipc`/`daemon` 模块与相关 CLI 经 `#[cfg(windows)]` 门控，
    核心（切块/reindex/检索）非 Windows 仍可编译
  - D3-A：daemon 仅 `ping`/`status`/`search`/`reload`；reindex 留 CLI（daemon 单线程串行，17+ min reindex 会阻塞所有查询）
  - D4-A：懒加载 + 进程内缓存（Embedder 键=model_dir，FtsTokenizer 键=(dict_dir, user_dict)）；`daemon --warm <lib>` 预热
  - D5-A：`search --daemon` 显式 flag（daemon 未运行 → `WaitNamedPipeW` 2s 预检快速报错）；默认本地行为不变
  - D6：单线程同步 accept 循环（`.single()` 独占实例；`PipeServer::disconnect()` 回 `ConnectingServer` 再 `wait()`）
  - `src/home.rs`：从 main.rs 移入 `program_dir`（去 clap 化）/`resolve_exe_symlinks`/`resolve_resource`/`resource_paths`（+2 单测随迁）
  - `src/search.rs`：`Output`→`pub SearchOutput`（serde 双向）；抽出 `search_core`（fts/embedder 由调用方提供；
    vector 分支校验 daemon 缓存 model_fp）与 `print_search_output`；CLI 包装在加载模型前先做 stale 预检（错误路径不白付 5.5s）
  - lindera 依赖：保持 path 依赖（crates.io 6.2.0 缺 `load_user_dictionary_for` 且词典 fmt 2≠3，
    切换需重编译词典 + 全量 reindex；7.0.0 未发布，等待无意义）
  - **验证**：`cargo test` 12/12（含新增 serde 往返 ×2、pipe_name 确定性/home 区分 ×1）；
    daemon `--warm demo`：本地 vs `search --daemon` text/JSON 逐字节一致（两条查询：纯向量 + BM25/vector 双路融合）；
    warm 首查 0.87s vs 本地 6.69s；`status`/`reload`/`ping` 正常；reload 后首查再 warm ~5.4s；
    daemon 未运行 → 「daemon 未运行或正忙」（2s 预检，退出码 1）

## 本次修复的问题

- 中断时的 25 个编译错误（借用/移动、`Statement::execute` 需 `&mut` 且须在
  `tx.commit()` 前 drop、rusqlite 无 tuple FromSql、tokenizers 0.23 的 `encode` 返回
  `Result` 等）
- HF `tokenizer.json` 自带 `truncation: {max_length: 128}`，`encode` 会静默截断
  → `with_truncation(None)` 清除（长文件测试只切出 1 块的根因）
- Rust `regex` 不支持 lookahead → 标题断点正则改写为无 lookaround 等价式
- yaml list 索引原为 1-based → 改为 0-based（与 JSON 侧一致）；渲染文本改为完整路径
  （`b.c: 2` 而非 `c: 2`）
- README：嵌入模型名更正为 `paraphrase-multilingual-MiniLM-L12-v2`（你已下载该模型，
  384d）；P0/P1 勾选项更新

## 待你定的决策

1. ~~FTS5 中文策略~~ → **已定 B**：lindera CC-CEDICT 词级分词（索引/查询端同分词）；
   词典从源码编译落地 `dict/cc-cedict/`
2. ~~rusqlite 版本~~ → **保持 0.32.1**（bundled，工作正常）
3. ~~嵌入窗口策略~~ → **定 C**：chunk 改 512 token（仅配置）；每块恰好落满窗口、整块嵌入无截断
   （最接近 qmd 语义：qmd 默认模型 embeddinggemma-300M 窗口 2048 ≥ 900 块）；默认与 demo 库
   `max_tokens` 已改 512，demo 库已全量重建
4. ~~BM25 查询构造~~ → **定 OR**：lindera 词项 OR 连接（高召回，BM25 加性打分天然把多词命中排前）；
   v1 无 LLM 查询扩展兜底，AND 易零结果；参考：qmd 用 AND（配 LLM 扩展）、WeKnora/hindsight 仅 rank-based RRF
5. ~~config.json 字段必填性~~ → **定：加默认**：serde 逐字段默认值（`config.rs` 的 `d_*` 函数 + 各结构体 `Default` impl，与 `lib add` 输出一致），仅 `id`/`root` 必填；新增 2 个配置单测（最小配置取全默认、部分覆盖），`cargo test` 6/6 通过
6. ~~README 完整性~~ → 本次补齐：新增「代码结构（部分→源文件）」「构建与运行」「CLI」「index.db schema」「检索输出格式」节；目录树补 testdata/userdict.csv/bin 探针、标注 ONNX-Q 未启用；修正"检索参数可省略"的旧表述；程序目录解析：exe 为符号链接时跟链到真实路径（`main.rs` `resolve_exe_symlinks`，2 个单测）
7. ~~暴力向量扫描性能直接实测~~ → **定 A，已实现**：新增 `src/bin/probe_scan.rs`（同 probe_onnx 模式：
   读 vectors 表、复制到 10 万 chunk 计时；cargo 自动发现，不改 Cargo.toml）。实测（libgraphify，5591 chunks）：
   debug 30.08ms（5591）/ 517.82ms（10 万）；release 2.92ms（5591）/ 50.23-51.42ms（10 万）。
   README 原"10 万 chunk <20ms"声称未达标（release 实测 ~51ms），README 已修正
8. ~~是否跑 libhindsight 全量 reindex~~ → **跳过**（外推足够）：按文件数 1046/395≈2.6×、按字节 18.64/7.26≈2.6×，
   graphify 17.3 min → 预计 ~45 min（~1.45 万 chunks），不必实测
9. ~~模型/词库自由指定~~ → **定 A+B+C，已实现**（见「已完成」）：A/B 如前；
   C 用 lindera 内置 `load_user_dictionary_for`（`load_dictionary` 已把词典目录 `metadata.json` 解析进 `Dictionary.metadata`，无需另读），
   用户词典 CSV 按主词典自身 `user_dictionary_schema` 解析（**非跨词典通用标准**：CC-CEDICT surface/part_of_speech/pinyin，ipadic/unidic surface/part_of_speech/reading，各自独立）

## 构建警告修复（2026-10-31）

- **LNK4098（LIBCMT 冲突）已根治**：根源是 libsearch 直接依赖 	okenizers = "0.23" 用了默认 features → saxx_fast = ["esaxx-rs/cpp"] →
  esaxx-rs build.rs .static_crt(true) 把 esaxx.cpp 编成 /MT 静态 CRT（dumpbin：esaxx.lib 带 /DEFAULTLIB:LIBCMT + /DEFAULTLIB:libcpmt），
  与 Rust/其余库的 /MD 冲突。esaxx C++ 核心只被 tokenizers unigram **训练**路径使用（suffix vs suffix_rs），本项目从不训练。
- **修复**：Cargo.toml → 	okenizers = { version = "0.23", default-features = false, features = ["onig"] }（与 fastembed 的依赖方式一致）。
  验证：release 重建无 LNK4098；demo 库 vector 路 / BM25 双路检索正常；cargo test --release 12/12。
- **遗留（待你定）**：libsearch/daemon/probe_onnx 三个 exe 仍会打印 warning: linker stdout: 正在创建库 ...lib 和对象 ...exp——
  onig 源码（regenc.h/regposerr.c）无条件定义 ONIGURUMA_EXPORT → MSVC 下 163 个 Onig 符号带 dllexport → exe 带导出表 → link 生成 .lib/.exp。
  无功能影响。选项：① 不管；② 三个 bin 加 #![allow(linker_messages)] 抑制显示；③ 编译时定义 ONIG_STATIC（需 CFLAGS 环境变量或 patch onig_sys）移除导出。
## 模型配置与 model pull（2026-10-31）

- **背景**：方便配置/更换嵌入模型——`model pull` 经 fastembed 预设或任意 HF repo（含量化模型 repo）拉取模型；
  ONNX 外部数据文件自动处理；pooling 不进配置。
- **决策（已确认）**：
  - 无 pooling 配置字段（`EmbeddingConfig` 保持 `{dir}`）；运行时推导：模型目录名 → `EmbeddingModel::from_str`
    （大小写不敏感）→ `TextEmbedding::get_default_pooling_method`；非预设名/查不到 → `Mean`（现有模型行为不变）
  - model_fp 新公式 `目录名|pooling|sha256(tokenizer.json)`（旧公式 `MODEL_NAME|sha256:...` 且 MODEL_NAME 为硬编码常量）；
    现有库升级后首次 reindex 全量重编码（用户接受："这只是测试"，用 demo 小库验证 stale 机制）
  - ONNX 发现：递归；优先标准名 `model.onnx`（唯一时）→ 否则仅一个 `.onnx` → 否则报错（现有模型目录有 9 个
    .onnx：Xenova repo 含多个量化变体，标准名优先保住旧 `onnx/model.onnx` 行为）；同目录 `<模型文件>.data`
    自动加载为 ONNX 外部数据（`UserDefinedEmbeddingModel::with_external_initializer`）
  - `model pull` 两形式互斥：`<preset>` vs `--repo org/name --file 路径`；`--dir`/`--extra` 可选；
    尊重 `HF_ENDPOINT`/`HF_TOKEN`；拒绝覆盖已存在非空目录；下载后实际建 ORT session 验证并打印 dim/pooling
- **实现**：
  - Cargo.toml：`hf-hub 0.5`（ureq + native-tls，default-features off）；`ureq 3`（仅用 Error 类型区分 404，同版本去重）
  - 新 `src/model.rs`：`pooling_for` / `pooling_name` / `PullSpec` / `pull`（+ 3 单测）
  - `src/embed.rs`：`find_onnx` 自动发现 + `with_pooling(pooling_for)` + 外部数据自动加载
  - `src/model_tokenizer.rs`：移除 `MODEL_NAME` 常量；`fingerprint` 新公式（+ 1 单测）
  - `src/main.rs`：`model pull` 子命令（互斥校验在 CLI 与 pull 内双重）
- **验证**：
  - `cargo test --release` 16/16 通过
  - demo 库：reindex 后 4 文件全量重编码（stale 机制生效）；DB 指纹 `paraphrase-multilingual-MiniLM-L12-v2|mean|sha256:2c33…`；检索正常
  - 量化模型（手工克隆目录，单 `model_optimized.onnx`）：临时库 reindex + 检索通过；
    指纹 `paraphrase-multilingual-MiniLM-L12-v2-onnx-Q|mean|sha256:fa68…`
  - `model pull ParaphraseMLMiniLML12V2Q` 端到端：下载 5 文件（model_optimized.onnx + 4 tokenizer 文件）、
    `.data` 404 跳过、验证加载 dim=384 pooling=mean；重复执行正确报"拒绝覆盖"
  - 参数校验：缺 `--file`、两形式混用、未知预设名均正确报错
- **已知边界**：
  - 手工克隆量化版目录名（`paraphrase-multilingual-MiniLM-L12-v2-onnx-Q`）非预设变体名 → pooling 回退 Mean；
    真实预设 `ParaphraseMLMiniLML12V2Q` 表中亦为 Mean，行为一致（换 Cls 类模型手工克隆时需人工核对 pooling）
  - HF repo 外部数据文件名非 `<模型文件>.data` 的少数情况：`--extra` 拉附加文件 + 人工放置到 onnx 同目录

## 下一步

- **P4（v2）daemon 模式**：engine 库化 + 命名管道服务 + 模型/索引预加载（模型加载 ~5.5s/查询是检索延迟主瓶颈）
