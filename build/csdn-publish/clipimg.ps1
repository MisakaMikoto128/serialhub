param([string]$Path)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($Path)
$bmp = New-Object System.Drawing.Bitmap -ArgumentList $img.Width, $img.Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::White)
$g.DrawImage($img, 0, 0, $img.Width, $img.Height)
$img.Dispose()
[System.Windows.Forms.Clipboard]::SetImage($bmp)
Write-Output "CLIPPED: $Path"
