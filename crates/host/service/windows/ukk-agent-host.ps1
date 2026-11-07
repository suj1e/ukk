# Explicit, user-scoped logon autostart for the ukk local Agent Host.
# Run as the interactive user; this is not a system service and never obtains
# elevated privileges. Stopping or restarting the Host never signals
# herdr-core or the Agents it owns.
param(
    [string]$ExePath = "{{EXE}}"
)
$ErrorActionPreference = 'Stop'
$taskName = 'ukk-agent-host'
$action = New-ScheduledTaskAction -Execute $ExePath -Argument 'serve'
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
Write-Output "Registered scheduled task '$taskName' at logon for $env:USERNAME."
Write-Output "herdr-core Agents are unaffected by Host stop/restart."
