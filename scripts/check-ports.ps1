# 端口占用审计 / 孤儿进程清理
#
# 用途：在改动 Rust 或做启动测试之前，确认没有上一次测试残留的 DSH 进程占着端口。
# 背景：DSH 在端口被占用（EADDRINUSE）时会直接退出、不会自动换端口，
#       残留进程会让你误判为「外壳启动失败」。
#
# 用法：
#   pwsh -File scripts/check-ports.ps1              # 只审计
#   pwsh -File scripts/check-ports.ps1 -KillTest    # 审计并清理非 3080 的 DSH 进程

[CmdletBinding()]
param(
  # 正式服务端口；该端口上的进程永远不会被本脚本结束。
  [int]$ProductionPort = 3080,
  # 清理除正式端口以外的 DSH 进程。
  [switch]$KillTest
)

$testRangeStart = 3090
$testRangeEnd = 3110

function Get-DshProcesses {
  Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
    $_.CommandLine -and (
      $_.CommandLine -match 'apps[/\\]cli[/\\]src[/\\]bin\.ts' -or
      $_.CommandLine -match 'corepack[/\\]dist[/\\]pnpm\.js"?\s+dsh' -or
      $_.CommandLine -match 'dsh\s+web'
    )
  }
}

Write-Host "=== 监听中的端口 ($ProductionPort, $testRangeStart-$testRangeEnd) ===" -ForegroundColor Cyan
$listeners = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
  Where-Object { $_.LocalPort -eq $ProductionPort -or ($_.LocalPort -ge $testRangeStart -and $_.LocalPort -le $testRangeEnd) }

if (-not $listeners) {
  Write-Host "  (无)" -ForegroundColor Green
} else {
  foreach ($l in $listeners) {
    $proc = Get-CimInstance Win32_Process -Filter "ProcessId=$($l.OwningProcess)" -ErrorAction SilentlyContinue
    $tag = if ($l.LocalPort -eq $ProductionPort) { '正式实例(保留)' } else { '测试残留' }
    $color = if ($l.LocalPort -eq $ProductionPort) { 'Yellow' } else { 'Red' }
    Write-Host ("  {0}:{1}  pid={2}  {3}  [{4}]" -f $l.LocalAddress, $l.LocalPort, $l.OwningProcess, $proc.Name, $tag) -ForegroundColor $color
  }
}

Write-Host ""
Write-Host "=== DSH 相关进程 ===" -ForegroundColor Cyan
$dsh = Get-DshProcesses
if (-not $dsh) {
  Write-Host "  (无)" -ForegroundColor Green
} else {
  foreach ($p in $dsh) {
    $cl = if ($p.CommandLine) { $p.CommandLine.Substring(0, [Math]::Min(120, $p.CommandLine.Length)) } else { '' }
    Write-Host ("  pid={0} ppid={1} {2}" -f $p.ProcessId, $p.ParentProcessId, $p.Name)
    Write-Host ("      $cl") -ForegroundColor DarkGray
  }
}

if ($KillTest) {
  Write-Host ""
  Write-Host "=== 清理非 $ProductionPort 端口的 DSH 进程 ===" -ForegroundColor Cyan

  # 保护范围 = 正式端口监听进程 + 它的全部祖先进程。
  # 只保护监听进程本身是不够的：DSH 的启动链是
  #   powershell -> corepack(node) -> cmd.exe -> node(bin.ts, 监听端口)
  # 若把祖先 corepack/cmd 杀掉，监听进程会变成孤儿，用户终端里的
  # `pnpm dsh web` 也会失去父子关系、无法再被 Ctrl+C 正常收尾。
  $protected = New-Object System.Collections.Generic.HashSet[int]
  foreach ($l in ($listeners | Where-Object { $_.LocalPort -eq $ProductionPort })) {
    $cursor = [int]$l.OwningProcess
    $guard = 0
    while ($cursor -gt 0 -and $guard -lt 32) {
      if (-not $protected.Add($cursor)) { break }
      $proc = Get-CimInstance Win32_Process -Filter "ProcessId=$cursor" -ErrorAction SilentlyContinue
      if (-not $proc) { break }
      $cursor = [int]$proc.ParentProcessId
      $guard++
    }
  }

  # 需要清理的 pid：监听测试端口的进程，以及不在保护链上的 dsh 进程。
  $toKill = New-Object System.Collections.Generic.HashSet[int]
  foreach ($l in ($listeners | Where-Object { $_.LocalPort -ne $ProductionPort })) {
    [void]$toKill.Add([int]$l.OwningProcess)
  }
  foreach ($p in $dsh) {
    if (-not $protected.Contains([int]$p.ProcessId)) { [void]$toKill.Add([int]$p.ProcessId) }
  }

  if ($toKill.Count -eq 0) {
    Write-Host "  (无需清理)" -ForegroundColor Green
  } else {
    # 注意：不要用 $pid 作循环变量——它是 PowerShell 只读自动变量。
    foreach ($targetPid in $toKill) {
      # 二次确认：绝不触碰正式端口的监听进程及其祖先链。
      if ($protected.Contains($targetPid)) {
        Write-Host "  跳过 pid=$targetPid（属于 $ProductionPort 的启动链）" -ForegroundColor Yellow
        continue
      }
      Stop-Process -Id $targetPid -Force -ErrorAction SilentlyContinue
      Write-Host "  已结束 pid=$targetPid" -ForegroundColor Red
    }
    Start-Sleep -Seconds 2
  }

  Write-Host ""
  Write-Host "=== 清理后复查 ===" -ForegroundColor Cyan
  $after = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Where-Object { $_.LocalPort -ge $testRangeStart -and $_.LocalPort -le $testRangeEnd }
  if ($after) {
    foreach ($l in $after) { Write-Host ("  仍占用: {0}" -f $l.LocalPort) -ForegroundColor Red }
  } else {
    Write-Host "  测试端口段已全部释放" -ForegroundColor Green
  }
  $prod = Get-NetTCPConnection -LocalPort $ProductionPort -State Listen -ErrorAction SilentlyContinue
  Write-Host ("  正式端口 {0}: {1}" -f $ProductionPort, $(if ($prod) { '仍在监听(未受影响)' } else { '未监听' })) -ForegroundColor Yellow
}
