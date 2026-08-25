# Delve 集成：ttd-gdbserver 作为可逆调试后端

[English](delve-integration.md)

Delve 侧的改动不在本项目维护，直接改在 delve 仓库
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve)
（`ttd-dbg` 分支）里。

集成目标：使 Windows 上的 TTD trace 获得与 Linux 上 `dlv replay`（rr）

与 rr 的一个刻意差异：rr 还支持对运行中的进程现场录制（`dlv exec
--backend=rr` 会先在 `rr record` 下启动程序再转入回放）。ttd-gdbserver
只做回放——录制由独立的 TTD 录制器（`ttd.exe`）承担，因此 Delve 在该后端
上拒绝 launch 类请求并提示先使用录制器。
同等的可逆调试体验。

Delve 只是 GDB RSP 的一个客户端——GDB、LLDB 同样支持反向调试，都能连接
ttd-gdbserver。本文件只讲 Delve 这条已验证的接入路径；协议选择的意义见
根目录 README.md。

## 工作原理

```
dlv ttd-gdbserver foo.run ──► gdbserial.TtdGdbserverReplay()
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
