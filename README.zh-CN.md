# ttd-gdbserver

[English](README.md)

**Windows 上的 rr。** 通过 GDB Remote Serial Protocol 回放 WinDbg TTD 轨迹，
让 GDB、LLDB 和 Delve 都能做时间旅行调试。一个 Rust 二进制：把调试器接到它
上面，无需打开 WinDbg 即可回放录制。

- **Windows 上的反向调试**：对任意 `.run` 轨迹向后单步/继续，restart 到任意
  位置。
- **兼容任意 GDB RSP 客户端**：GDB、LLDB、Delve；Go 调试经 Delve 端到端验证。
- **只读回放（x86_64）**：不写内存和寄存器。

## 快速开始

```bash
# 服务一个会话后退出
ttd-gdbserver <trace.run> [--listen 127.0.0.1:1234]
# 持续接受重连直到 Ctrl+C（位置、断点、checkpoint 跨会话保留）
ttd-gdbserver <trace.run> --accept-multiclient
```

服务器同一时间只服务一个会话。Ctrl+C 停止接受新连接，等当前会话结束，释放
trace 后以退出码 0 退出；再按一次强制退出。

配合带 TTD 后端的 `dlv`（见[与 Delve 集成](#与-delve-集成)）：

```bash
dlv ttd <trace.run>
```

## 与 Delve 集成

Delve 是第一个接入、也是目前唯一端到端验证过的客户端。Delve 侧的改动在
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve) 的
`ttd-dbg` 分支；客户端契约见
[`doc/delve-integration.md`](doc/delve-integration.md)。

可逆命令：`step`、`next`、`stepout`、`step-instruction` 及其 `rev` 反向版本，
以及 `restart [pos]`。

## 工作原理

```text
GDB / LLDB / Delve ──GDB RSP──►  协议前端
                                    │
                                调试目标层
                                    │
                                TTD Replay DLL ──► .run trace
```

前端只负责协议，目标层只提供指令级操作：执行控制、内存读、断点（用 TTD
观察点模拟）、线程与寄存器访问、位置导航。没有符号层——服务端回放的是录制
文件，不解释你的程序。设计决策见
[`doc/architecture.md`](doc/architecture.md)；TTD 回放 API 提供什么、如何映射
到本项目见 [`doc/ttd-internals.md`](doc/ttd-internals.md)（英文）。

### 没有唯一的"当前时刻"

live 调试器会把所有线程停下，于是所有线程共享同一个瞬间，"读这个地址现在
是什么"只有一个答案。录制没有这样的瞬间：它只有一条由单步组成的时间线，而
光标停在**某个线程的某一步**上。客户端问的一切，都是相对这一步回答的。

有两点后果，读栈之前值得先知道：

- **寄存器是按线程的、且精确的。** `thread 2` 再 `bt`，用的是线程 2 的最后
  一步，而不是"与线程 1 同一时刻"。这正是录制诚实的读法：这两个线程并没有
  停在一个共同点上。
- **内存是"找回"的，不是"重放"的。** 一次读取返回的是索引中该地址最新的
  数据行：对停住的那个线程最近碰过的地址（它的栈、它的局部变量）是精确的，
  而对只有别的线程在很久以前写过的地址，就可能是个更旧的值。这个值仍然是
  录制中真实存在过的值——只是不一定等于光标位置那一刻的值。

所以有用的规则是：**信寄存器，其次信当前栈帧，再次信最近被触碰过的内存；
其余的宁可用观察点。** 写观察点会停在执行那次写操作的指令上，此刻新值精确，
再往回单步就恢复旧值。`monitor ttd mem` 会打印返回的每一段字节来自哪个
trace 位置，可疑的读取可以据此排查，而不是靠猜。

### 观察点是优势，不是妥协

GDB 的 `Z2`–`Z4`（写 / 读 / 访问）在 live 进程上是硬件调试寄存器：四个、
按线程分配，且只支持有限宽度、对齐的数据。在这里它们是 TTD 的内存观察点
事件，因此得到：

- **几乎没有数量限制**——引擎回放数据流并报告命中的访问，而不是靠比较器
  触发陷阱；
- **两个方向都精确命中**——`reverse-continue` 能到达同一地址的上一次写入，
  硬件观察点做不到；
- **读观察点不再是二等公民**——在硬件上它们与写观察点共用那四个 DR 槽，
  且只覆盖 1/2/4/8 字节对齐范围；这里的读命中来自引擎对每条已执行加载的
  认知，代价与写观察点相同，且覆盖任意范围。

在 `tests/target_watchpoint_hits.rs` 中验证：写命中停下时已是新值，
`step-back` 则恢复旧值。

## GDB RSP 协议面

多进程（单进程 pid）、all-stop。

- **寄存器 / 内存 / 线程**：`g`/`G`、`p`/`P`（P 返回 `E`）、`m`/`M`（M 返回
  `E`）、`H`、`qfThreadInfo`/`qsThreadInfo`、`qThreadExtraInfo`。
- **执行**：`vCont;c`、`vCont;s`、`vCont;r`（范围单步）、`s`/`c`、`bc`/`bs`
  （反向）、`vRun`（restart 到指定位置）、`D`。
- **断点**：`Z0`/`Z1`（用 TTD 观察点模拟）、`Z2`–`Z4` 数据观察点。
- **内省**：`qXfer:features:read`、`qXfer:exec-file:read`、`qXfer:auxv:read`、
  `qXfer:libraries:read`、`qOffsets`、`qGetTLSAddr`、`qSupported`、`qAttached`、
  `qRcmd`。
- **刻意不支持**：`qXfer:memory-map`（GDB 会把它当成完整可读区域，从而拒绝
  读栈/TEB）、`QNonStop`、fork/vfork 事件、启动配置（回放没有 "spawn"）、
  `QListThreadsInStopReply`。

回放只读（`M`、`P`、`X` 返回 `E`）；信号不会投递——录制的信号流是唯一有效的。
trace 中从未出现过的线程 id 没有状态，查询返回 `E`。

### `monitor ttd …`

| 命令 | 输出 |
|---|---|
| `monitor ttd info` | trace 生命周期、当前位置、索引状态、模块/线程/异常计数 |
| `monitor ttd threads` | 全部曾经存在过的线程（UTID、OS tid、活跃区间） |
| `monitor ttd module <addr>` | 地址 → 模块 + 区间 |
| `monitor ttd mem <addr> [len]` | 字节，以及每一段字节取自哪个 trace 位置（默认 16 字节，上限 256） |
| `monitor ttd events [n]` | 录制的异常事件（代码、地址、pc、位置；默认 20 条） |
| `monitor ttd stats` | 查询数、光标定位次数、单步数、回放次数、物理观察点添加/删除次数 |

## 构建

Windows x86_64、MSVC 和 TTD 回放 SDK；`cargo build` 生成单个二进制。

GDB RSP 引擎是 [gdbstub](https://github.com/lwintermelon/gdbstub)，以 `ttd-dbg`
分支的 git 依赖引入——不在 crates.io 上，因为该分支承载了本服务需要的协议
扩展。

TTD 边界是 [`cxx`](https://cxx.rs) bridge：`src/ttd/cpp/ttd_cxx.hpp` /
`ttd_cxx.cpp` 持有 SDK 句柄并暴露类型化的 C++ 接口，`src/ttd/ffi.rs` 是桥，
`src/ttd/` 是其余代码使用的安全封装。所有权、回调、内存 slice 和普通值结构体
都走 bridge，Rust 侧不再有 bindgen 生成的 raw FFI 层。上游
[`ttd`](https://github.com/hugsy/ttd-rust) 绑定是起点，但它没有调试器前端需要
的观察点回调、中断、内存策略、区间查询和按线程取上下文；塑造这层接口的约束见
[`doc/ttd-internals.md` §13](doc/ttd-internals.md)（英文）。

## 测试

```powershell
cargo test            # 协议与单元测试；依赖 trace 的测试如实报告为 ignored
mise run test:rust    # 全量套件，使用 mise.toml [env] 提供的语料录制
```

依赖 trace 的测试读取 `TTD_TRACE_PATH`，异常测试读取 `TTD_FAULT_TRACE_PATH`。

### 端到端门禁

`tests/e2e/` 用真实客户端驱动服务端——Go 用 Delve，C/C++ 用 GDB——差分的是
整个 **transcript**，而不是对具体值做断言：

| 门禁 | 比较对象 | 出现差异意味着 |
|---|---|---|
| `forward` | 回放 vs 一次录制的 live 运行（地址、tid、位置归一化） | 回放偏离真实执行 |
| `reverse` | 同一 trace 的两次回放，不做任何掩码 | 反向单步不确定 |

脚本只负责导航和打印（`p/x $pc`、`bt`、`info locals`）：整份 transcript 就是
判据，回归会以 diff 行的形式出现，而不是被手写断言吃掉。`tests/e2e/programs/`
下的七个程序各自覆盖一组特性（清单见
[`tests/e2e/README.md`](tests/e2e/README.md)），它们的 `.dlv`/`.gdb` 就是普通
的命令脚本，也可以直接拿来交互式驱动。

```powershell
mise run test                                     # 全语料跑两道门禁
python tests/e2e/e2e.py forward --only go-core    # 单用例，更底层
```
