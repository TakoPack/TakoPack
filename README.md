# TakoPack

TakoPack 是一个用于将各种语言生态系统的软件（目前支持 Rust/Cargo）打包为 Linux 发行版 RPM spec 文件的工具。

## 功能特性

- **单包打包**: 为单个 crate 生成 RPM spec 文件
- **批量打包**: 从文本文件批量处理多个 crate
- **本地打包**: 直接从本地 Cargo.toml 生成 spec 文件
- **依赖追踪**: 追踪 crate 依赖关系并生成处理列表
- **依赖解析**: 自动解析和生成所有依赖的 spec 文件

## 安装

```bash
cargo install --path .
```

或从源码构建：

```bash
cargo build --release
```

## 使用方法

### 主要命令

TakoPack 的 Rust/Cargo 操作都在 `cargo` 子命令下：

#### 1. pkg - 打包单个 Crate

从 crates.io 下载并为单个 crate 生成 RPM spec 文件。

```bash
# 打包指定版本
takopack cargo pkg <CRATE_NAME> <VERSION>

# 打包最新版本
takopack cargo pkg <CRATE_NAME>

# 使用自定义配置
takopack cargo pkg <CRATE_NAME> <VERSION> --config config.toml

# 示例
takopack cargo pkg serde 1.0.210
takopack cargo pkg tokio
```

**输出**: 创建 `rust-{crate}-{version}/rust-{crate}.spec` 目录，只包含 spec 文件。所有临时文件（源码、tar 文件等）会自动清理。

**特点**:
- 自动下载指定版本的 crate
- 生成符合 RPM 规范的 spec 文件
- 自动提取许可证和元数据信息
- 处理特性（feature）依赖

#### 2. localpkg - 本地打包

从本地目录或 Cargo.toml 文件直接生成 spec 文件，无需下载。适用于开发中的项目或自定义的 crate。

```bash
# 从目录打包（目录需包含 Cargo.toml）
takopack cargo localpkg <PATH>

# 从 Cargo.toml 文件打包
takopack cargo localpkg path/to/Cargo.toml

# 指定输出目录
takopack cargo localpkg <PATH> -o output_dir

# 示例
takopack cargo localpkg ./my-project
takopack cargo localpkg ./Cargo.toml -o specs/
```

**输出**: 在当前目录或指定的输出目录中创建 `rust-{crate}.spec` 文件。

**特点**:
- 无需上传到 crates.io 即可生成 spec
- 适合本地开发和测试
- 支持路径为目录或直接指向 Cargo.toml 文件
- 自动处理本地依赖关系

#### 3. batch - 批量打包

从文本文件批量处理多个 crate，适合大规模打包场景。

```bash
# 基本用法
takopack cargo batch <FILE>

# 指定输出目录
takopack cargo batch <FILE> -o output_dir

# 示例
takopack cargo batch crates.txt -o batch_output/
```

**文件格式**: 文本文件每行一个 crate，格式为 `crate_name version`
```text
# crates.txt 示例
serde 1.0.210
tokio 1.35.0
clap 4.5.0
# 注释行以 # 开头
anyhow 1.0.75
```

**输出**: 创建目录（带时间戳或指定名称），包含所有 crate 的 spec 文件：
```
batch_output/
├── rust-serde/
│   └── rust-serde.spec
├── rust-tokio/
│   └── rust-tokio.spec
├── rust-clap/
│   └── rust-clap.spec
└── ...
```

**特点**:
- 支持批量处理多个 crate
- 自动跳过空行和注释
- 提供详细的成功/失败统计
- 错误处理：单个失败不影响其他 crate 的处理

#### 4. track - 依赖追踪

追踪 crate 的依赖关系，生成需要处理的 crate 列表，并自动批量打包新的依赖。这是一个智能的依赖管理工具，维护一个本地数据库来避免重复处理。

```bash
# 从 crate 名称追踪
takopack cargo track <CRATE_NAME> [VERSION]

# 从 Cargo.toml 文件追踪
takopack cargo track -f path/to/Cargo.toml

# 从 Cargo.lock 文件追踪
takopack cargo track -f path/to/Cargo.lock

# 指定输出目录和数据库路径
takopack cargo track <CRATE_NAME> -o output_dir --database custom_db.txt

# 示例
takopack cargo track pyo3 0.26.0
takopack cargo track -f ./Cargo.toml -o deps/
takopack cargo track -f my-project/Cargo.lock
```

**工作流程**:
1. **解析依赖**: 分析指定 crate/文件的所有依赖关系
2. **数据库比对**: 与本地数据库比较，识别新的依赖
3. **自动打包**: 批量打包所有新识别的依赖 crate
4. **更新数据库**: 记录已处理的 crate 信息

**输出**: 
- 创建带时间戳的目录（如 `track_20260124_140708/`），包含所有新 crate 的 spec 文件
- 更新本地数据库（默认：`~/.config/takopack/crate_db.txt`）
- 显示详细的分析报告

**支持的输入模式**:
- **模式 1**: Crate 名称 + 版本（从 crates.io 下载）
- **模式 2**: Cargo.toml 文件（自动生成 Cargo.lock）
- **模式 3**: Cargo.lock 文件（直接解析）

**数据库功能**:
- 自动维护已处理 crate 的记录
- 避免重复打包相同的依赖
- 支持自定义数据库路径
- 集成 Git 自动提交功能（需启用 `back_db` 特性）

**分析报告示例**:
```
📊 Analysis Results:
  - Total packages in dependency graph: 156
  - Database entries before: 120
  - Database entries after: 156
  - New entries added: 36
  - Crates needing processing: 36

🆕 Crates that will be processed:
    1) ✓ syn v2.0.48
    2) ✓ quote v1.0.35
    3) ✓ proc-macro2 v1.0.76
    ...

🚀 Starting batch package...
```

**特点**:
- 智能依赖追踪和去重
- 自动检测文件格式（Cargo.toml 或 Cargo.lock）
- 批量自动打包新依赖
- 持久化数据库管理
- 详细的处理统计和错误报告

## 命令别名

为了方便使用，主要命令提供了以下简短别名：

- `package` → `pkg`
- `localpkg` → `local`

## 使用示例

### 示例 1: 打包单个 Crate

```bash
# 打包 serde 1.0.210
takopack cargo pkg serde 1.0.210
```

输出结构：
```
rust-serde-1.0/
└── rust-serde.spec
```

### 示例 2: 批量打包多个 Crate

创建 `crates.txt`：
```text
serde 1.0.210
tokio 1.35.0
clap 4.5.0
```

执行批量打包：
```bash
takopack cargo batch crates.txt -o my_packages/
```

输出结构：
```
my_packages/
├── rust-serde-1.0/
│   └── rust-serde.spec
├── rust-tokio-1.0/
│   └── rust-tokio.spec
└── rust-clap-4.0/
    └── rust-clap.spec
```

### 示例 3: 本地项目打包

```bash
# 为当前项目生成 spec
takopack cargo localpkg ./Cargo.toml

# 为另一个项目生成 spec
takopack cargo localpkg ../other-project -o specs/
```

### 示例 4: 依赖追踪和批量处理

```bash
# 追踪 pyo3 的所有依赖
takopack cargo track pyo3 0.26.0

# 从本地项目追踪依赖
takopack cargo track -f ./Cargo.toml -o project-deps/

# 使用自定义数据库
takopack cargo track actix-web 4.0 --database ~/my_db.txt
```

输出示例：
```
✓ Detected Cargo.lock format (by content)
✓ Using existing lockfile
Parsing dependencies...
✓ Parsed 156 packages from dependency graph

📊 Analysis Results:
  - Total packages in dependency graph: 156
  - Database entries before: 120
  - Database entries after: 156
  - New entries added: 36
  - Crates needing processing: 36

🆕 Crates that will be processed:
    1) ✓ syn v2.0.48
    2) ✓ quote v1.0.35
    ...

🚀 Starting batch package...
Output directory: track_20260124_140708

[1/36] Processing: syn 2.0.48
  ✓ Successfully packaged syn 2.0.48
[2/36] Processing: quote 1.0.35
  ✓ Successfully packaged quote 1.0.35
...

================================================================
Batch Processing Summary
================================================================
Total packages processed: 36
Successfully packaged:    35
Failed:                   1
================================================================
```

## 输出格式

所有生成的 spec 文件遵循 RPM spec 格式，包含：

- 正确的 `crate()` provides/requires 声明
- 来自 Cargo 依赖的版本约束
- 正确处理特性（feature）依赖
- 自动提取许可证和元数据

## 环境变量

- `RUST_LOG`: 设置日志级别（例如：`RUST_LOG=debug takopack cargo pkg serde`）

## Future Support

Takopack is designed to support multiple language ecosystems:

- ✅ Rust/Cargo (currently implemented)
- 🚧 Perl/CPAN (planned)
- 🚧 Python/PyPI (planned)
- 🚧 Go modules (planned)
## 工作流程建议

### 典型使用场景

1. **新项目打包**: 使用 `pkg` 命令打包单个 crate
2. **依赖完整性**: 使用 `track` 命令追踪和处理所有依赖
3. **批量处理**: 使用 `batch` 命令从列表批量打包
4. **本地开发**: 使用 `localpkg` 命令测试本地项目

### 推荐工作流

```bash
# 1. 追踪项目依赖并生成数据库
takopack cargo track -f ./Cargo.lock -o deps/

# 2. 后续只需打包新的 crate（track 会自动识别）
takopack cargo track -f ./Cargo.lock

# 3. 或者直接批量打包指定的 crate 列表
takopack cargo batch crates.txt -o batch_output/
```

## 数据库管理

TakoPack 使用本地数据库（默认位于 `~/.config/takopack/crate_db.txt`）来追踪已处理的 crate，避免重复工作。

- 自动创建和更新
- 记录每个 crate 的名称、版本和兼容性信息
- 支持 Git 版本控制（需启用 `back_db` 特性）

## 许可证

本项目采用 MIT 许可证。

## 贡献

欢迎贡献！请随时提交 issue 和 pull request。
