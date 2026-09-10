# Windows发送端分段诊断

这是Windows端默认开启必要发送计量的诊断包，不是突破60FPS的修复包。保留现有QoS、补sleep、ACK模式、编码器、分辨率、画质和背压行为；不会自动升级已有Windows安装、扩大权限、开启全量敏感日志或修改系统设置。

## 启动

直接双击本包的`rustdesk.exe`即可，不需要PowerShell启动命令或环境变量。不要用此文档自动终止系统服务或其他远程连接。

发送计量在该构建的Windows进程中始终开启；非Windows构建不启用。无参数GUI会先尝试连接已有主IPC服务器；成功时只同步配置，新界面不代表会话由新包承载。不要强行另启`--server`或自动停止已有服务。

核对同目录BUILD-METADATA.json的source与交付SHA256SUMS。只有出现同一build和PID的`sender_trace service`/`sender_trace transport`实际发送计量，才能确认诊断到达承载会话的进程。可用任务管理器只读核对该PID的程序路径。没有诊断行只能判为日志被过滤、没有发送、仍由旧IPC服务承载或进程归属未确认，不能记成0FPS。

release使用现有文件日志，默认级别可记录info，无需额外设置RUST_LOG。已有RUST_LOG若过滤掉info则需先由操作者检查。Windows通常在RustDesk配置根旁的`log`目录；按承载角色还可能在`log\server`或`log\portable-service`。不要混入其他用户、旧包或其他会话日志。

连接前将客户端期望FPS设为120，保持原分辨率、H265及连续动态画面，不改Windows显示模式。尽量只保留这个测试观看连接；多个观看者的请求会共同影响旧QoS，不要自动断开他人连接。收集至少60秒稳定区间，同时记录客户端R14原生计量。不要以启动首秒、静态桌面或重新连接窗口作为稳态FPS。

在本包目录的PowerShell中，可以只导出本构建的诊断行。`LogRoot`由操作者选择实际承载进程的日志目录，不扫描整个用户目录、不上传原始日志：

```powershell
$build = (Get-Content '.\BUILD-METADATA.json' -Raw | ConvertFrom-Json).source
if ($build -notmatch '^[0-9a-f]{40}$') { throw 'Invalid build metadata' }
$LogRoot = Read-Host 'Actual RustDesk log directory'
$pattern = "sender_trace (service|transport|qos_event) pid=\d+ build=$build "
$rows = @(Get-ChildItem -LiteralPath $LogRoot -File -Recurse -Filter '*.log' |
    Select-String -Pattern $pattern |
    ForEach-Object { $_.Line.Substring($_.Line.IndexOf('sender_trace ')) })
if ($rows.Count -eq 0) { throw 'No matching sender samples; do not report 0 FPS' }
$out = "sender-trace-$(Get-Date -Format 'yyyyMMdd-HHmmss').txt"
$rows | Set-Content -LiteralPath $out -Encoding utf8
```

导出后再按PID和测试时间选取同一承载进程的稳态区间；旧日志轮转文件也可能匹配同一个构建。不要把不同运行批次拼成一段。

## 数据口径

计量区分请求FPS、QoS实际调度、采集得到的新帧、编码输出、发送入队尝试、transport send返回成功。现有ServiceTmpl返回的是尝试发送的订阅目标集合，不验证每个通道send成功，因此不能把该集合大小记成成功入队。一次消息可以包含多个EncodedVideoFrame记录，不能用消息数或bool替代其数量；这仍不是客户端已显示帧数。

每个聚合使用实际经过时间，而不是无论停顿多久都按一秒计算；窗口帧率为`encoded_frames * 1000 / elapsed_ms`。成功transport send也不证明客户端已收讫、解码或显示。不同线程窗口边界可能不同；按时间范围对齐，不能把两个相近日志行当成同一个帧的端到端延迟。跨设备还需要核对时钟偏差，不能直接相减Windows墙钟和客户端单调计时来声称端到端延迟。

日志只需要诊断字段，不上传完整RustDesk日志、身份、服务器配置、口令、画面或压缩码流。日志仍由既有日志系统落盘，默认计量有少量计时和每秒日志开销。最终性能验收需要量化这部分开销；如需无计量A/B，由开发侧提供专用构建，不要求用户设置环境变量。

### 日志字段与限制

- `sender_trace service`：按display的采集/编码服务窗口。`qos_fps`依次是latest/min/max，`spf_ms`是最近调度间隔；`capture_calls/ok/would_block/errors`和各阶段`total_ms/max_ms`用于定位时间消耗。`encoded_frames`只计编码输出的EncodedVideoFrame记录。
- `dispatch_attempt_messages/frames`按订阅目标数累计发送尝试；多连接时会高于单份编码输出。它们不保证通道入队成功。
- `sender_trace transport`且`scope=connection_all_displays`：按匿名connection序号累计socket发送。单display窗口的`window_display`为其数值；多display混合或无法归属时为`-1`（`display_sentinel_minus1=mixed_or_none`）。多display不能把整连接累计量当成单display帧率。
- `sender_trace qos_event`：custom/auto FPS被接受后的请求值、当时实际QoS值和允许上限，不代表后续整个窗口保持该值。
- `closed=1 partial=1`是连接退出时强制flush的尾窗，可能不足一秒，也可能因停止发送后等待关闭而更长；时长始终看`elapsed_ms`。失败-only窗也保留。正常稳态计算应先明确选定区间，不把尾窗与完整窗简单算术平均。

服务端SWITCH/error等提前退出的不足一秒服务窗口可能没有flush；缺行不能证明零错误或零工作。`empty=1`表示该服务窗口没有capture/encode调用，不能由此认定编码器容量为0。真实吞吐必须保留区间内的停顿时间，不能删掉空窗来抬高FPS。

## 判别

1. QoS实际FPS小于等于60：优先检查RustDesk主动调度，而非只检查编码器初始化FPS。
2. QoS为120而采集/编码/transport仅60：按阶段耗时和WouldBlock定位，180Hz显示器不保证每秒180个不同桌面帧。
3. 实际transport明显超过70、客户端持续积压但native输出约60：发送端限速不能单独解释，应继续隔离native解码/Surface回收。

现有Windows GPU后端仍有额外纹理复制/转换等严格零拷贝缺口。本次没有增加像素搬运，不代表全链已经零拷贝。

## 结束

结束本次测试时正常退出本包程序即可。本包不写永久环境变量；诊断只增加前述安全聚合行，不自动导出或上传完整日志。无需运行停服务脚本。

本地纯计量测试通过不等于Windows集成编译或NVENC实测通过；构建验证结果以对应CI和交付记录为准。
