$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName Microsoft.VisualBasic
$path = $env:GX_RECYCLE_PATH
if (![System.IO.File]::Exists($path)) { throw '照片文件不存在' }
$item = Get-Item -LiteralPath $path -Force
if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { throw '不能回收链接文件，请在资源管理器中处理' }
$drive = New-Object System.IO.DriveInfo([System.IO.Path]::GetPathRoot($path))
if (!$drive.IsReady -or $drive.DriveType -ne [System.IO.DriveType]::Fixed) {
    throw '此磁盘无法确认支持回收站，请在资源管理器中处理（本程序不执行永久删除）'
}
[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile(
    $path,
    [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs,
    [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin,
    [Microsoft.VisualBasic.FileIO.UICancelOption]::ThrowException)
if ([System.IO.File]::Exists($path)) { throw '文件未移入回收站' }
