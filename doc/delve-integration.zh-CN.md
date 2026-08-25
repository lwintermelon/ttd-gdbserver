# Delve 集成：ttd-gdbserver 作为可逆调试后端

[English](delve-integration.md)

Delve 侧的改动不在本项目维护，直接改在 delve 仓库
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve)
（`ttd-dbg` 分支）里。

集成目标：使 Windows 上的 TTD trace 获得与 Linux 上 `dlv replay`（rr）
同等的可逆调试体验。

Delve 只是 GDB RSP 的一个客户端——GDB、LLDB 同样支持反向调试，都能连接
ttd-gdbserver。本文件只讲 Delve 这条已验证的接入路径；协议选择的意义见
根目录 README.md。

## 工作原理

```
dlv ttd foo.run ──► gdbserial.TtdReplay()
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
$env:DELVE_TTD_SERVER = "<本项目>\target\debug\ttd-gdbserver.exe"

# 3. 录制（任意 Windows 进程，用 TTD 自带工具；需管理员权限）
ttd.exe myapp.exe

# 4. 用 Delve 回放调试 Go 程序的 trace
dlv.exe ttd <trace.run> --exe <被录制的可执行文件>
```

现成的 Go 示例程序与 `init.txt` 命令脚本在 `examples/` 目录——完整的
编译/录制/回放流程见根目录 README 的"自己动手体验"一节。

进入 Delve 终端后，全部可逆命令可用：

| 命令 | 说明 |
|---|---|
| `continue` / `step` / `next` / `stepout` | 正向执行 |
| `reverse-continue` / `rev step` / `rev next` / `rev stepout` | 反向执行（走 `bc`/`bs`） |
| `restart` / `restart <pos>` | 跳转到 trace 开头 / 指定位置 |
| （自动） | 每次停止后自动显示当前位置（`<SEQ>:<STEPS>` 16 进制；`state.When` ← `qRRCmd when`，无独立 `when` 命令） |

注：后端只读——不支持写内存/寄存器，函数调用注入（call injection）不可用。
