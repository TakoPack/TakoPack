# TakoPack

TakoPack 是一个用于将各种语言生态系统的软件（目前支持 Rust/Cargo 和 Python/PyPI）打包为 Linux 发行版 RPM spec 文件的工具。

## 功能特性

- **单包打包**: 为单个 crate 生成 RPM spec 文件
- **本地打包**: 直接从本地 Cargo.toml 生成 spec 文件
- **Python 打包**: 为单个 Python 包生成 RPM spec 文件
- **Registry 同步**: 从 ruyispec 同步 crate 到本地 registry，支持增量更新和并发下载
- **依赖检查**: 验证 crate 能否从本地 registry 解析依赖
- **BuildRequires 生成**: 自动生成 RPM 的 BuildRequires 声明

## 安装

```bash
cargo install --path crates/takopack
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

当前保留的 Cargo 子命令包括 `pkg`、`localpkg`、`registry-sync`、`resolve-check` 和 `buildreqs`。批量文件处理、vendor 递归入口、Cargo.toml 递归解析入口以及手动 crates.io 缓存刷新入口已不再提供。

#### 1. pkg - 打包单个 Crate

从 crates.io 下载并为单个 crate 生成 RPM spec 文件。

```bash
# 打包指定版本
takopack cargo pkg <CRATE_NAME> <VERSION>

# 打包最新版本
takopack cargo pkg <CRATE_NAME>

# 指定输出目录
takopack cargo pkg <CRATE_NAME> <VERSION> --directory output_dir

# 临时输出 TakoPack 内置 SPDX 头
takopack cargo pkg <CRATE_NAME> <VERSION> --with-spdx

# 示例
takopack cargo pkg serde 1.0.210
takopack cargo pkg tokio
```

**输出**:
- 默认（无 `--directory`）：创建 `rust-{crate}-{compat_version}/` 目录
- 指定 `--directory output_dir`：创建 `output_dir/rust-{crate}-{compat_version}/` 目录

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

# 临时输出 TakoPack 内置 SPDX 头
takopack cargo localpkg <PATH> --with-spdx

# 示例
takopack cargo localpkg ./my-project
takopack cargo localpkg ./Cargo.toml -o specs/
```

**输出**:
- 默认（无 `-o`）：在当前目录创建 `rust-{crate}-{compat_version}/` 子目录，包含 spec 和 Cargo.toml
- 指定 `-o output_dir`：创建 `output_dir/rust-{crate}-{compat_version}/` 子目录，包含 spec 和 Cargo.toml

**特点**:
- 无需上传到 crates.io 即可生成 spec
- 适合本地开发和测试
- 支持路径为目录或直接指向 Cargo.toml 文件
- 自动处理本地依赖关系

#### 3. registry-sync - 同步 Registry

从 ruyispec 仓库同步 Rust crate 到本地 Cargo registry 目录。用于构建本地离线 registry，供 `resolve-check` 和 `buildreqs` 使用。

```bash
# 同步（使用配置文件中的路径）
takopack cargo registry-sync

# 试运行，只显示计划，不实际修改
takopack cargo registry-sync --dry-run

# 指定并发数（默认 8）
takopack cargo registry-sync -j 4
```

**参数**:
- `--dry-run`: 只打印同步计划，不修改文件
- `-j, --jobs N`: 并发下载/解压的线程数，默认 8

**输出示例**:
```
Registry sync
  ruyispec: /path/to/openruyi-repo
  registry: /path/to/cargo-registry
  jobs: 8

Summary:
  add=5
  update=2
  remove=0
  skip=100
  warnings=0
  sync_errors=0
```

**特点**:
- 增量同步：通过 SHA-256 hash 对比，只更新变化的 crate
- 并发下载：多线程并行下载，提高效率
- 安全机制：marker 文件防止误操作非托管目录
- 原子更新：先写临时目录，再 rename 替换

#### 4. resolve-check - 依赖解析检查

验证单个 Cargo crate 能否使用本地 registry 完成依赖解析。用于检查本地 registry 的完整性。

```bash
# 检查当前目录的 crate
takopack cargo resolve-check .

# 检查指定路径
takopack cargo resolve-check ./path/to/crate

# 指定 registry 目录（覆盖配置文件）
takopack cargo resolve-check . --registry /path/to/registry
```

注：请使用此命令来检查仓库使用 `crate` 构建的应用包的依赖是否满足。

**输出示例**:
```
Resolve check
  manifest: /path/to/crate/Cargo.toml
  registry: /path/to/cargo-registry

Result: ok
```

**返回值**:
- `0`: 解析成功，所有依赖都在本地 registry 中
- `1`: 解析失败，有依赖缺失

#### 5. buildreqs - 生成 BuildRequires

从 Cargo 依赖解析结果自动生成 RPM 的 BuildRequires 声明。

```bash
# 为当前目录的 crate 生成 BuildRequires
takopack cargo buildreqs .

# 为指定路径生成
takopack cargo buildreqs ./path/to/crate

# 指定 registry 目录
takopack cargo buildreqs . --registry /path/to/registry
```

**输出示例**:
```
BuildRequires:  crate(serde-1) >= 1.0.210
BuildRequires:  crate(tokio-1) >= 1.40.0
BuildRequires:  crate(anyhow-1) >= 1.0.86
```

**特点**:
- 自动过滤：只输出 registry 来源的依赖，排除本地路径依赖
- 版本兼容：使用 compat version 规则（如 `1.x.y` → `1`）
- 去重排序：自动去重并按字母顺序排列

注：目前输出的构建依赖比较冗长，可以考虑后续结合 `feature` 进行缩减。

## 配置文件

TakoPack 使用 `takopack.toml` 配置文件来设置默认路径。

### 配置文件位置

按以下顺序查找，找到第一个即停止：

1. `./takopack.toml`（当前工作目录）
2. `~/.config/takopack/takopack.toml`（Linux）
3. `~/Library/Application Support/takopack/takopack.toml`（macOS）
4. `C:\Users\{user}\AppData\Roaming\takopack\takopack.toml`（Windows）

### 配置项

```toml
[ruyispec]
# ruyispec 仓库路径，registry-sync 命令需要
local_path = "/path/to/openruyi-repo"

[registry]
# 本地 Cargo registry 目录
# 可选，默认为 $XDG_DATA_HOME/takopack/cargo-registry
local_path = "/path/to/cargo-registry"
```

### 相对路径

`local_path` 支持相对路径，相对于配置文件所在目录：

```toml
# 配置文件: /home/user/project/takopack.toml
[ruyispec]
local_path = "../openruyi-repo"  # 实际路径: /home/user/openruyi-repo
```

### 默认 registry 路径

如果未配置 `[registry].local_path`，使用以下默认路径：

- Linux: `~/.local/share/takopack/cargo-registry`
- macOS: `~/Library/Application Support/takopack/cargo-registry`
- Windows: `C:\Users\{user}\AppData\Roaming\takopack\cargo-registry`

### 配置示例

```toml
[ruyispec]
local_path = "/root/git/openruyi-repo"

[registry]
local_path = "/root/git/takopack-cargo-registry"
```

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
