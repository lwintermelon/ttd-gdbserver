# ttd-gdbserver

[English](README.md)

**Windows 上的 rr。** 通过 GDB Remote Serial Protocol 回放 WinDbg TTD 轨迹，
让 GDB、LLDB、Delve 都能做时间旅行调试。一个 Rust 二进制：接上你的调试器，
无需打开 WinDbg 即可回放。

- **Windows 上的反向调试** —— 可对任意 `.run` 轨迹向后单步/继续，restart 到任意位置。
- **兼容任意 GDB RSP 客户端** —— GDB、LLDB、Delve；Go 程序调试已通过 Delve 端到端验证。
- **只读回放（x86_64）** —— 不写内存/寄存器，无需重跑即可检查同一次执行。

## 快速开始

```bash
# GDB RSP 前端（GDB / LLDB / Delve）
ttd-gdbserver <trace.run> [--listen 127.0.0.1:1234] [--exe <可执行文件>]
```

服务器同一时间只接受一个会话；引擎状态跨会话保留。

配合 Delve 回放调试（需先从 fork 构建带 TTD 后端的 `dlv`，见
[与 Delve 集成](#与-delve-集成)）：

```bash
dlv ttd <trace.run> --exe <被录制的可执行文件>
```

## 为什么选择 GDB Remote Serial Protocol

GDB RSP 是调试器与调试后端之间的通用协议，支持面很广：GDB、LLDB、Delve
都内置 GDB RSP 客户端，且都支持反向调试。选择它意味着 ttd-gdbserver 是
一个通用机制——就像 Linux 上的 rr 一样，任何会说 GDB RSP 的调试器都能
接上来做时间旅行调试，而不是绑定某个特定前端。

协议本身很古老、谈不上现代，但它是事实标准：rr 正是通过 GDB RSP 为 GDB
提供反向调试，Delve 的 rr 后端（`pkg/proc/gdbserial`）也是现成的 GDB RSP
客户端。复用这条成熟链路，Delve 侧只需极小改动即可获得与 rr 一致的可逆
调试体验。

目前项目已通过 Delve 完成 Go 程序的实际调试（见下节），说明基本可用。

## 与 Delve 集成

Delve 是第一个接入的客户端（Go 程序调试已验证可用）。Delve 侧的改动在
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve) 的
`ttd-dbg` 分支，见 [`doc/delve-integration.md`](doc/delve-integration.md)。

快速开始（从该分支构建带 TTD 后端的 dlv 后）：

```bash
dlv ttd <trace.run> --exe <被录制的可执行文件>
```

支持的可逆命令（Delve 终端）：`step` / `next` / `stepout` / `step-instruction`
及其 `rev` 反向版本、`restart [pos]`。

## 自己动手体验

`examples/` 目录有一个小型 Go 程序（`examples/go-sample/`）和一份
`init.txt` 命令脚本，覆盖断点、单步、反向执行、restart。
完整复现流程：

```powershell
# 1. 编译示例 Go 程序（需要 Go；-N -l 关闭优化，局部变量才可读）
cd examples\go-sample
go build -gcflags="all=-N -l" -o sample.exe .

# 2. 用 TTD 录制（需要管理员权限；ttd.exe 是 TTD 自带的录制工具）
ttd.exe sample.exe
#    → 生成 sample.run（首次回放时生成 sample.idx）

# 3. 用 Delve 回放调试，用 init.txt 驱动会话
$env:DELVE_TTD_SERVER = "<本项目>\target\debug\ttd-gdbserver.exe"
$env:TERM = "dumb"
dlv.exe ttd sample.run --exe sample.exe --init ..\init.txt
```

`init.txt` 脚本跑完自动退出。也可以去掉 `--init` 参数交互式体验——全部
可逆命令（`rev step`、`reverse-continue`、`restart` 等）都可用。

## 架构

```text
GDB / LLDB / Delve ──GDB RSP──► src/gdb/ (GdbTarget)
                                          │ DebugTarget trait
                                     src/target/ (TtdProcess)
                                          │
                                     src/ttd/ (FFI) ──► TTD Replay DLL
```

GDB 前端构建在目标层 `DebugTarget` trait 之上（`src/target/mod.rs`）：
指令级调试操作（正/反向执行、内存读、断点/观察点、线程/寄存器、
位置导航），无符号层概念。

## GSP 协议面

本 stub 是一个多进程（单进程 pid）all-stop 的 GDB RSP 服务器，实现了
gdb / LLDB / Delve 在 Windows 回放场景下期望的标准报文，外加少量回放
专属约定：

- **寄存器 / 内存 / 线程**：`g`/`G`、`p`/`P`（P 返回 `E`）、`m`/`M`
  （M 返回 `E`）、`H`、`qfThreadInfo`/`qsThreadInfo`、`qThreadExtraInfo`。
- **执行**：`vCont;c`/`vCont;s`/`vCont;r`（范围单步）、`s`/`c`、
  `bc`/`bs`（反向）、`vRun`（restart 到指定位置）、`D`（detach）。
- **断点**：`Z0`/`Z1`（用 TTD 观察点模拟）、`Z2`/`Z3`/`Z4` 数据观察点。
- **内省**：`qXfer:features:read`（target description XML）、
  `qXfer:exec-file:read`、`qXfer:auxv:read`（合成的 `AT_ENTRY`）、
  `qXfer:libraries:read`（PE 模块表）、`qXfer:memory-map:read`
  （模块对应 `ram` 区域）、`qOffsets`、`qGetTLSAddr`（TEB）、
  `qSupported`、`qAttached`、`qRcmd`（`monitor ttd …`）。
- **信号 / 模式**：`QPassSignals`/`QProgramSignals`/`vCtrlC` 返回 `OK`；
  无 `QNonStop`（仅 all-stop）、无 fork/vfork 事件、无启动配置报文——
  回放没有"spawn"。

### 回放与 live 调试的语义差异

回放只读：写内存/寄存器（`M`、`P`、`X`）返回 `E`。信号不会真正投递——
录制的信号流是唯一有效的。线程 continue（`vCont;c:tid`）等同于整条
trace 的 continue：其他线程只在 cursor 前进时随之推进，因为 TTD 回放在
宿主上是单线程的。这与 rr 对外声明的能力一致。

### TTD 自定义命令（RR 风格）

GDB 用户通过 `monitor` 获得时间旅行内省：

| 命令 | 输出 |
|---|---|
| `monitor ttd info` | trace 生命周期、当前位置、模块/线程/异常计数 |
| `monitor ttd threads` | 全部曾经存在过的线程（UTID、OS tid、活跃区间） |
| `monitor ttd module <addr>` | 将地址解析到模块 + 区间 |
| `monitor ttd events [n]` | 异常事件摘要（完整列表见后续 `qTTDEvents`） |


## 构建

```powershell
$env:TTD_SDK_DIR = "<Microsoft.TimeTravelDebugging.Apis NuGet 包目录>"
cargo build
```

依赖 Windows 上的 TTD SDK 与 MSVC。TTD SDK 是 NuGet 包
`Microsoft.TimeTravelDebugging.Apis`（含 C++ 头文件与 `TTDReplay.lib`）；
TTD Replay API 的使用见 [WinDbg-Samples 的 TTD 目录](https://github.com/microsoft/WinDbg-Samples/tree/master/TTD)。
安装后得到包目录，用环境变量 `TTD_SDK_DIR` 指向它，`build.rs` 从
`sdk\include` / `sdk\lib\x64` 推导路径。运行时 DLL（`TTDReplay.dll` 等）
随 TTD 安装获得，把其目录加入 `PATH`。

GDB RSP 引擎基于 [gdbstub](https://github.com/lwintermelon/gdbstub) 的
`ttd-dbg` 分支（git 依赖；内含三个小补丁，均另以上游 PR 提交）；x86-64
寄存器文件复用同分支的 `gdbstub_arch`（`X86_64CoreRegs`，GPR + x87/SSE）。

## 测试

### 环境准备

TTD 运行时 DLL（`TTDReplay.dll` 等）需在 `PATH` 中；集成测试还需要一个
`.run` trace（`TTD_TRACE_PATH` 指向它）。

### 单元与集成测试（77 个）

| 测试 | 数量 | 需要 TTD？ |
|---|---|---|
| `cargo test --lib`（断点管理、停止原因映射、XML 转义） | 22 | 否 |
| `cargo test --test gdb`（GDB RSP 握手/可逆命令、mock 后端、GSP 协议面） | 24 | 否 |
| `cargo test --test target`（TtdProcess 目标层） | 15 | 是（无 trace 自动跳过） |
| `cargo test --test gdb_ttd`（GDB RSP + 真实 trace） | 4 | 是（无 trace 自动跳过） |
| `cargo test --test ttd`（TTD FFI 层） | 12 | 是（必需） |

```powershell
# 无 TTD trace（46 个）
cargo test --lib --test gdb

# 有 TTD trace（全部 77 个）
$env:TTD_TRACE_PATH = "<path.run>"
cargo test
```

### 端到端测试（dlv ttd）

用 Delve 回放调试 Go 程序 trace（需先从 Delve fork 构建带 TTD 后端的 dlv，
见 `doc/delve-integration.md`）：

```powershell
$env:DELVE_TTD_SERVER = "<本项目>\target\debug\ttd-gdbserver.exe"
$env:TERM = "dumb"
dlv.exe ttd <trace.run> --exe <被录制的可执行文件> --init <脚本>
```

`TERM=dumb` 是必须的——否则长输出会触发 Delve 的分页器，吞掉脚本输入。
