$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Windows.Forms;
public sealed class PickerWindow : IWin32Window {
    public IntPtr Handle { get; private set; }
    public PickerWindow(long handle) { Handle = new IntPtr(handle); }
}
'@
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$owner = New-Object System.Windows.Forms.Form
try {
    $dialog.Description = '选择文件夹'
    $dialog.ShowNewFolderButton = $true
    if ($env:PVM_PICKER_INITIAL -and [System.IO.Directory]::Exists($env:PVM_PICKER_INITIAL)) {
        $dialog.SelectedPath = $env:PVM_PICKER_INITIAL
    }
    # A shown, invisible owner keeps the native dialog above the application.
    # TopMost is scoped to this short-lived owner; it is disposed on every exit.
    $owner.Text = '照片管理器 - 选择文件夹'
    $owner.ShowInTaskbar = $false
    $owner.StartPosition = 'CenterScreen'
    $owner.Size = New-Object System.Drawing.Size(1, 1)
    $owner.Opacity = 0
    $owner.TopMost = $true
    $handleValue = 0L
    if ([long]::TryParse($env:PVM_PICKER_OWNER, [ref]$handleValue) -and $handleValue -ne 0) {
        $parent = New-Object PickerWindow($handleValue)
        $owner.Show($parent)
    } else {
        $owner.Show()
    }
    $owner.Activate()
    if ($dialog.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) {
        [Console]::Out.Write($dialog.SelectedPath)
    }
} finally {
    $dialog.Dispose()
    $owner.Close()
    $owner.Dispose()
}
