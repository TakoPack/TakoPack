# TakoPack

TakoPack 是一个用于将各种语言生态系统的软件（目前支持 Rust/Cargo 和 Python/PyPI）打包为 Linux 发行版 RPM spec 文件的工具。

## 功能特性

- **单包打包**: 为单个 crate 生成 RPM spec 文件
- **本地打包**: 直接从本地 Cargo.toml 生成 spec 文件
- **Python 打包**: 为单个 Python 包生成 RPM spec 文件

## 安装

```bash
cargo install --path .
```

或从源码构建：

```bash
cargo build --release
```

## 使用方法

### Python 命令

TakoPack 的 Python/PyPI 操作在 `py` 子命令下：

```bash
# 打包最新版本
takopack py package <NAME>

# 打包指定版本
takopack py package <NAME> <VERSION>

# 指定输出目录
takopack py package <NAME> -o output_dir
```

Python 功能已内置：使用 `takopack py package <NAME> [VERSION] [-o output_dir]` 即可生成 Python 包对应的 RPM spec（默认输出到 `python-{srcname}/python-{srcname}.spec`）。

### Cargo 命令

TakoPack 的 Rust/Cargo 操作都在 `cargo` 子命令下：

#### 1. pkg - 打包单个 Crate

从 crates.io 下载并为单个 crate 生成 RPM spec 文件。

```bash
# 打包指定版本
takopack cargo pkg <CRATE_NAME> <VERSION>

# 打包最新版本
takopack cargo pkg <CRATE_NAME>

# 指定输出目录
takopack cargo pkg <CRATE_NAME> <VERSION> --directory output_dir

# 使用自定义配置
takopack cargo pkg <CRATE_NAME> <VERSION> --config config.toml

# 示例
takopack cargo pkg serde 1.0.210
takopack cargo pkg tokio
```

**输出**:
- 默认（无 `--directory`）：创建 `rust-{crate}-{compat_version}/` 目录
- 指定 `--directory output_dir`：将 spec 和 Cargo.toml 直接放入 `output_dir/`

目录内容：
- `rust-{crate}-{compat_version}.spec` - RPM spec 文件
- `Cargo.toml` - 归一化的 Cargo.toml 文件

版本兼容性规则（compat_version）：
- `1.x.y` → `1`（主版本兼容）
- `0.x.y` → `0.x`（次版本兼容，x > 0）
- `0.0.x` → `0.0.x`（补丁版本兼容）
- 预发布版本使用完整版本号（如 `0.26.0-beta.1`）

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

**输出**:
- 默认（无 `-o`）：在当前目录创建 `rust-{crate}-{compat_version}/` 子目录，包含 spec 和 Cargo.toml
- 指定 `-o output_dir`：将 spec 和 Cargo.toml 直接放入 `output_dir/`，不创建子目录

**特点**:
- 无需上传到 crates.io 即可生成 spec
- 适合本地开发和测试
- 支持路径为目录或直接指向 Cargo.toml 文件
- 自动处理本地依赖关系

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
rust-serde-1/
├── rust-serde-1.spec
└── Cargo.toml
```
### 示例 2: 本地项目打包

```bash
# 为当前项目生成 spec
takopack cargo localpkg ./Cargo.toml

# 为另一个项目生成 spec
takopack cargo localpkg ../other-project -o specs/
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
- ✅ Python/PyPI (currently implemented)
- 🚧 Go modules (planned)

## 许可证

本项目采用 MIT 许可证。

## 贡献

欢迎贡献！请随时提交 issue 和 pull request。
