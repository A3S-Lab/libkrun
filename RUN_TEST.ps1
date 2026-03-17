# 测试诊断和运行脚本 - Test Diagnostic and Run Script
# 此脚本必须在真实的Windows PowerShell环境中运行

Write-Host "╔══════════════════════════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║          LAPIC改进测试 - LAPIC Improvement Test             ║" -ForegroundColor Cyan
Write-Host "╚══════════════════════════════════════════════════════════════╝" -ForegroundColor Cyan
Write-Host ""

# 1. 环境检查
Write-Host "=== 1. 环境检查 ===" -ForegroundColor Yellow
Write-Host ""

Write-Host "检查PowerShell版本..." -ForegroundColor Gray
$PSVersionTable.PSVersion
Write-Host ""

Write-Host "检查当前目录..." -ForegroundColor Gray
Write-Host "当前目录: $PWD"
Write-Host ""

Write-Host "检查可执行文件..." -ForegroundColor Gray
$exePath = ".\target\release\examples\test_kernel_boot.exe"
if (Test-Path $exePath) {
    Write-Host "✅ 找到可执行文件: $exePath" -ForegroundColor Green
    $exeInfo = Get-Item $exePath
    Write-Host "   大小: $($exeInfo.Length) bytes"
    Write-Host "   修改时间: $($exeInfo.LastWriteTime)"
} else {
    Write-Host "❌ 未找到可执行文件: $exePath" -ForegroundColor Red
    Write-Host "   请先运行: cargo build --release --example test_kernel_boot"
    exit 1
}
Write-Host ""

Write-Host "检查libkrunfw.dll..." -ForegroundColor Gray
$dllPath = ".\target\release\libkrunfw.dll"
if (Test-Path $dllPath) {
    Write-Host "✅ 找到libkrunfw.dll: $dllPath" -ForegroundColor Green
    $dllInfo = Get-Item $dllPath
    Write-Host "   大小: $($dllInfo.Length) bytes"
} else {
    Write-Host "❌ 未找到libkrunfw.dll: $dllPath" -ForegroundColor Red
    Write-Host "   请先运行: cd src\libkrunfw-win && cargo build --release"
    exit 1
}
Write-Host ""

# 2. 设置环境
Write-Host "=== 2. 设置环境 ===" -ForegroundColor Yellow
Write-Host ""

$env:RUST_LOG = "info"
$env:PATH = "$PWD\target\release;$env:PATH"

Write-Host "✅ RUST_LOG = $env:RUST_LOG" -ForegroundColor Green
Write-Host "✅ PATH已更新,包含: $PWD\target\release" -ForegroundColor Green
Write-Host ""

# 3. 运行测试
Write-Host "=== 3. 运行测试 ===" -ForegroundColor Yellow
Write-Host ""
Write-Host "测试将运行10秒,然后自动停止" -ForegroundColor Gray
Write-Host "输出将保存到: test_result.log" -ForegroundColor Gray
Write-Host ""
Write-Host "开始测试..." -ForegroundColor Green
Write-Host ""

$outputFile = "test_result.log"
$errorFile = "test_result_err.log"

try {
    # 启动进程
    $process = Start-Process -FilePath $exePath `
        -NoNewWindow `
        -PassThru `
        -RedirectStandardOutput $outputFile `
        -RedirectStandardError $errorFile

    Write-Host "进程已启动 (PID: $($process.Id))" -ForegroundColor Gray

    # 等待10秒
    $waited = 0
    while ($waited -lt 10 -and -not $process.HasExited) {
        Start-Sleep -Seconds 1
        $waited++
        Write-Host "." -NoNewline -ForegroundColor Gray
    }
    Write-Host ""

    # 停止进程
    if (-not $process.HasExited) {
        Write-Host "停止进程..." -ForegroundColor Yellow
        $process.Kill()
        $process.WaitForExit()
        Write-Host "✅ 进程已停止" -ForegroundColor Green
    } else {
        Write-Host "⚠️  进程已自行退出 (退出码: $($process.ExitCode))" -ForegroundColor Yellow
    }
} catch {
    Write-Host "❌ 运行测试时出错: $_" -ForegroundColor Red
    exit 1
}

Write-Host ""

# 4. 分析结果
Write-Host "=== 4. 分析结果 ===" -ForegroundColor Yellow
Write-Host ""

# 检查输出文件
if (Test-Path $outputFile) {
    $outputLines = Get-Content $outputFile -ErrorAction SilentlyContinue
    Write-Host "输出文件: $outputFile ($($outputLines.Count) 行)" -ForegroundColor Gray
} else {
    Write-Host "⚠️  未找到输出文件: $outputFile" -ForegroundColor Yellow
    $outputLines = @()
}

if (Test-Path $errorFile) {
    $errorLines = Get-Content $errorFile -ErrorAction SilentlyContinue
    Write-Host "错误文件: $errorFile ($($errorLines.Count) 行)" -ForegroundColor Gray
} else {
    $errorLines = @()
}

Write-Host ""

# 合并输出
$allOutput = $outputLines + $errorLines

if ($allOutput.Count -eq 0) {
    Write-Host "❌ 没有任何输出!" -ForegroundColor Red
    Write-Host ""
    Write-Host "可能的原因:" -ForegroundColor Yellow
    Write-Host "1. 程序立即崩溃" -ForegroundColor Gray
    Write-Host "2. DLL依赖问题" -ForegroundColor Gray
    Write-Host "3. 权限问题" -ForegroundColor Gray
    Write-Host ""
    Write-Host "建议:" -ForegroundColor Yellow
    Write-Host "1. 在真实的Windows PowerShell中运行此脚本" -ForegroundColor Gray
    Write-Host "2. 不要在Git Bash或WSL中运行" -ForegroundColor Gray
    Write-Host "3. 确保有管理员权限" -ForegroundColor Gray
    exit 1
}

Write-Host "✅ 获得 $($allOutput.Count) 行输出" -ForegroundColor Green
Write-Host ""

# 显示前50行
Write-Host "=== 前50行输出 ===" -ForegroundColor Cyan
$allOutput | Select-Object -First 50 | ForEach-Object { Write-Host $_ }
Write-Host ""

# 5. 关键模式分析
Write-Host "=== 5. 关键模式分析 ===" -ForegroundColor Yellow
Write-Host ""

# 查找LAPIC消息
$lapicMessages = $allOutput | Select-String -Pattern "LAPIC" -AllMatches
if ($lapicMessages) {
    Write-Host "✅ 找到 $($lapicMessages.Count) 条LAPIC消息" -ForegroundColor Green
    Write-Host ""
    Write-Host "LAPIC消息 (前10条):" -ForegroundColor Cyan
    $lapicMessages | Select-Object -First 10 | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
} else {
    Write-Host "⚠️  未找到LAPIC消息" -ForegroundColor Yellow
}
Write-Host ""

# 查找STUCK消息
$stuckMessages = $allOutput | Select-String -Pattern "STUCK" -AllMatches
if ($stuckMessages) {
    Write-Host "⚠️  找到 $($stuckMessages.Count) 条STUCK消息" -ForegroundColor Red
    Write-Host ""
    Write-Host "STUCK消息 (前10条):" -ForegroundColor Cyan
    $stuckMessages | Select-Object -First 10 | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
} else {
    Write-Host "✅ 未找到STUCK消息 (好消息!)" -ForegroundColor Green
}
Write-Host ""

# 查找Exit消息
$exitMessages = $allOutput | Select-String -Pattern "Exit #" -AllMatches
if ($exitMessages) {
    Write-Host "✅ 找到 $($exitMessages.Count) 条Exit消息" -ForegroundColor Green
    Write-Host ""
    Write-Host "Exit消息 (前10条):" -ForegroundColor Cyan
    $exitMessages | Select-Object -First 10 | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
    Write-Host ""
    Write-Host "Exit消息 (最后5条):" -ForegroundColor Cyan
    $exitMessages | Select-Object -Last 5 | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
} else {
    Write-Host "⚠️  未找到Exit消息" -ForegroundColor Yellow
}
Write-Host ""

# 6. 结论
Write-Host "=== 6. 测试结论 ===" -ForegroundColor Yellow
Write-Host ""

if ($lapicMessages -and -not $stuckMessages -and $exitMessages) {
    Write-Host "🎉 测试成功!" -ForegroundColor Green
    Write-Host ""
    Write-Host "✅ LAPIC寄存器正常工作" -ForegroundColor Green
    Write-Host "✅ 没有检测到卡住" -ForegroundColor Green
    Write-Host "✅ 内核正常执行" -ForegroundColor Green
    Write-Host ""
    Write-Host "下一步: 检查串口输出,监控启动进度" -ForegroundColor Cyan
} elseif ($stuckMessages) {
    Write-Host "⚠️  测试发现问题" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "内核仍然卡住,需要进一步分析" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "请查看 test_result.log 了解详情" -ForegroundColor Cyan
} else {
    Write-Host "⚠️  测试结果不确定" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "输出不完整或测试未正常运行" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "请查看 test_result.log 和 test_result_err.log" -ForegroundColor Cyan
}

Write-Host ""
Write-Host "完整日志文件:" -ForegroundColor Gray
Write-Host "  - test_result.log (标准输出)" -ForegroundColor Gray
Write-Host "  - test_result_err.log (错误输出)" -ForegroundColor Gray
Write-Host ""
Write-Host "╔══════════════════════════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║                    测试完成                                  ║" -ForegroundColor Cyan
Write-Host "╚══════════════════════════════════════════════════════════════╝" -ForegroundColor Cyan
