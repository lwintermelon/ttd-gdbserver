# Delve 集成：ttd-gdbserver 作为可逆调试后端

[English](delve-integration.md)

Delve 侧的改动不在本项目维护，直接改在 delve 仓库
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve)
（`ttd-dbg` 分支）里。

集成目标：使 Windows 上的 TTD trace 获得与 Linux 上 `dlv replay`（rr）
同等的可逆调试体验。

与 rr 的一个刻意差异：rr 还支持对运行中的进程现场录制（`dlv exec
--backend=rr` 会先在 `rr record` 下启动程序再转入回放）。ttd-gdbserver
只做回放——录制由独立的 TTD 录制器（`ttd.exe`）承担，因此 Delve 在该后端
上拒绝 launch 类请求并提示先使用录制器。

Delve 只是 GDB RSP 的一个客户端——GDB、LLDB 同样支持反向调试，都能连接
ttd-gdbserver。本文件只讲 Delve 这条已验证的接入路径。

## 工作原理

```
dlv ttd foo.run ──► gdbserial.TtdGdbserverReplay()
                        │  启动子进程: ttd-gdbserver foo.run --listen 127.0.0.1:0
                        │  解析 "Listening on 127.0.0.1:<port>"
                        ▼
                    gdbserial gdbProcess（Delve 现成的 GDB RSP 客户端，
                    与 rr 后端同一套代码）
                        │  GDB Remote Serial Protocol（TCP）
                        ▼
                    ttd-gdbserver（本项目 src/gdb/）
                        │  DebugTarget trait
                        ▼
                    TtdProcess ──► WinDbg TTD Replay API
```

协议选择 GDB RSP 的原因：Delve 的 rr 后端 `pkg/proc/gdbserial` 本身
就是一个 GDB RSP 客户端，已实现全部可逆调试语义（`bc`/`bs` 反向执行、
restart）。ttd-gdbserver 说同一种协议，Delve 侧改动最小。

## 构建与使用

```powershell
# 1. 克隆并构建带 TTD 后端的 dlv（ttd-dbg 分支）
git clone -b ttd-dbg https://github.com/lwintermelon/delve
cd delve
go build -o dlv.exe .\cmd\dlv

# 2. 让 dlv 能找到 ttd-gdbserver：放到 PATH，或
$env:DELVE_TTD_SERVER = "C:\path\to\ttd-gdbserver.exe"
#    额外的 ttd-gdbserver 参数可通过 DELVE_TTD_SERVER_FLAGS 注入
#    （对标 rr 的 DELVE_RR_REPLAY_FLAGS），例如查看协议层调试日志：
# $env:DELVE_TTD_SERVER_FLAGS = "-v"

# 3. 录制（任意 Windows 进程，用 TTD 自带工具；需管理员权限）
ttd.exe myapp.exe

# 4. 用 Delve 回放调试 trace（可执行文件由 ttd-gdbserver 经
#    qXfer:exec-file 报告，与 `dlv replay` 相同）：
dlv.exe ttd <trace.run>

#    可选：当录制的可执行文件不在原路径，或在非录制机器上调试时，
#    可以显式指定可执行文件：
dlv.exe ttd <trace.run> C:\path\to\matching\myapp.exe
```

现成的 Go/C 程序与其 Delve/GDB 命令脚本在 `tests/e2e/programs/` 目录：每个
目录都是一个完整的调试场景，脚本也可以直接交互式驱动（见
[`tests/e2e/README.md`](../tests/e2e/README.md)）。

进入 Delve 终端后，全部可逆命令可用：

| 命令 | 说明 |
|---|---|
| `continue` / `step` / `next` / `stepout` | 正向执行 |
| `reverse-continue` / `rev step` / `rev next` / `rev stepout` | 反向执行（走 `bc`/`bs`） |
| `restart` / `restart <pos>` | 跳转到 trace 开头 / 指定位置 |
| （自动） | 每次停止后自动显示当前位置（`<SEQ>:<STEPS>` 16 进制；`state.When` ← `qRRCmd when`，无独立 `when` 命令） |

注：后端只读——不支持写内存/寄存器，函数调用注入（call injection）不可用。

### 这里的 goroutine 视图意味着什么

Delve 从 runtime 元数据解析 goroutine：`gs_base` 是 TEB，TLS 槽与
`m.curg` 指向 `g`，goroutine 列表来自 `runtime.allgs`。这些都是"读内存"，
因此适用 [`ttd-internals.md` §7.1](ttd-internals.md) 的规则：

- **单线程的寄存器与栈是精确的。** Delve 为*停住的那个*线程报告的
  goroutine，就是录制中该线程这一步的答案。
- **已 park 的 goroutine，其自身状态通常是精确的**——它 park 时就不再变化，
  陈旧的数据行里仍是它的当前值。
- **光标停下时仍在别的线程上运行的 goroutine，才是可能显出陈旧的地方。**
  它的 `g` 字段是按光标位置读取的，而那一刻别的线程还在写这些字段。实践中
  受影响更多的是无锁的 runtime 结构——运行队列、netpoller、timer 堆——而不是
  用互斥锁或 channel 保护的结构，后者的更新发生在时间线忠实排序的
  sequence 事件上。
- 陈旧字段仍然是*录制中的一个值*，不是垃圾：它的形状 Delve 解析得出来，
  所以症状通常是"某个 goroutine 的状态慢了一拍"，而不是崩溃。

粗略的用法建议：真正停住的那个线程、任何经锁/channel 触达的数据、以及反向
执行，都可以信 trace；如果 goroutine 列表看起来不一致，先用
`monitor ttd mem <addr>` 核对每次读取的来源，再决定是否采信；某个字段的
历史确实重要时，用写断点。

### VSCode（逆向调试 GUI）

VSCode 的 Go 插件直接可用，对 recorded 目标会自动启用反向 continue /
反向 step 控件（Delve 在 capabilities 事件中报告 `supportsStepBack`）。

**场景一：连接已启动的回放服务器。** 先起 headless 服务器：

```powershell
dlv.exe ttd <trace.run> --headless -l 127.0.0.1:2345
```

launch.json：

```json
{
  "name": "go ttd attach",
  "type": "go",
  "request": "attach",
  "mode": "remote",
  "host": "127.0.0.1",
  "port": 2345
}
```

**场景二：从 VSCode 直接启动回放。** launch.json（`env` 传给 dlv 进程，
ttd-gdbserver 不在 PATH 时在这里指定）：

```json
{
  "name": "Replay TTD trace",
  "type": "go",
  "request": "launch",
  "mode": "ttd-gdbserver",
  "ttdTracePath": "${workspaceFolder}\\sample.run",
  "env": {
    "DELVE_TTD_SERVER": "C:\\path\\to\\ttd-gdbserver.exe"
  }
}
```

settings.json 指定一次带 TTD 后端的 dlv：

```json
{
  "go.alternateTools": {
    "dlv": "C:\\path\\to\\dlv.exe"
  }
}
```

编辑 launch.json 时 schema 对 `mode`/`ttdTracePath` 的 lint 提示可忽略。

## 服务器端兼容面

Delve 的 `pkg/proc/gdbserial` 客户端会逐字解析本服务的回复。下表汇总
所有因它而存在的服务器端决策（报文本身是通用的——`qRRCmd` 是 rr 的
词汇，`qGetTLSAddr` 和 `QPassSignals` 是标准 RSP；应答这些报文的模块
`src/gdb/target/unknown.rs` 是客户端中立的）：

| 行为 | 位置 |
|---|---|
| `qRRCmd when` / checkpoint / `vRun;c<N>` 重启 | `src/gdb/commands.rs`（在 `unknown.rs` 中应答） |
| `vRun`/`R` 重启失败返回错误回复，而不是静默成功 | `src/gdb/commands.rs` + `src/gdb/target/extended.rs` |
| 客户端在 `restart` 后紧接的 resume 停在重启点（rr 行为）；继续前向回放会丢掉 checkpoint | `src/gdb/target.rs`（`restart_pending`、`spawn_replay`） |
| `qGetTLSAddr` → TEB（GS base；不写 tracee 内存） | `src/gdb/target/unknown.rs` |
| `QListThreadsInStopReply` / `QThreadSuffixSupported` 拒绝（显式空回复） | `src/gdb/target/unknown.rs` |
| `QPassSignals` / `QProgramSignals` 接受但忽略（只读回放） | `src/gdb/target/unknown.rs` |
| `vCtrlC` 中止进行中的回放 | `src/gdb/target/unknown.rs` |
| `use_rle() = false`——Delve 的二进制解码器不支持 RLE | `src/gdb/target.rs` |
| `use_no_ack_mode() = true` | `src/gdb/target.rs` |
| 59 寄存器 `g` 布局 + `gs_base` = TEB，用于定位 goroutine `g` | `src/gdb/regs.rs` |
| 断点注册失败返回 `E16`（`Ok(false)` → `NonFatalError(22)`） | `src/gdb/target/breakpoints.rs` + gdbstub |
| ^C 报 `T02`——两个来源：协议中断 + 后端 `Interrupted` | `src/gdb/session.rs`（`on_interrupt`）与 `src/gdb/mapping.rs` |
| `k` → EOF 无回复（gdbstub fork 补丁），`D` → `OK` | `src/gdb/session.rs` |
| rr 风格 `T09`（trace 末尾）/ `T00`（trace 起点）停止词汇 | `src/gdb/mapping.rs` |
| 进程退出同样先报 `T09`（可倒回，对应 Delve 的 `almostExited`）；仅当客户端在退出停止点上再次*前向*执行时才答 `W00`——即 rr 的 `in_debuggee_end_state` → `handle_exited_state`，但去掉了 rr 对后续 resume 的 FATAL（这里幂等地回答 `W00`） | `src/gdb/target.rs`（`end_state`、`spawn_replay`） |
| 所有异常类在 continue/step 时都表现为停止（Delve 的 continue 循环会自动越过），但真实调试器会放过的 first-chance 码（`0xE06D7363`、`0x20474343`）除外——continue 会直接回放过去 | `src/target/process/execution.rs` |
| 回放期间的查询既不阻塞也不杀死会话；只有致命错误路径的 trait 方法回答中性的兜底值 | `src/gdb/target/base.rs`、`thread_info.rs`、`description.rs`、`extended.rs` |
| checkpoint（以及位置/断点）跨重连保持 | `src/gdb/session.rs` |
