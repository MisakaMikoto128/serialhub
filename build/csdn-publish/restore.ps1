Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);' -Name W -Namespace U
$h = (Get-Process -Id $args[0]).MainWindowHandle
[U.W]::ShowWindow($h, 9) | Out-Null
[U.W]::SetForegroundWindow($h) | Out-Null
Write-Output "RESTORED hwnd=$h"
