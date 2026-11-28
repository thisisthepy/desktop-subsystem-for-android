param([string]$Title, [string]$Out, [string]$Monitor = 'main')
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -Namespace D -Name A -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(System.IntPtr v);'
[D.A]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null
Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class M { public delegate bool P(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(P p, IntPtr l);
[DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
[DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
public struct R { public int L,T,Rt,B; } }
'@
$script:found = @()
[M]::EnumWindows({ param($h,$l) $sb = New-Object Text.StringBuilder 256; [M]::GetWindowText($h,$sb,256) | Out-Null; if ([M]::IsWindowVisible($h) -and $sb.ToString() -eq $Title) { $script:found += $h }; $true }, [IntPtr]::Zero) | Out-Null
if ($script:found.Count -eq 0) { "no window '$Title'"; exit 1 }
$h = $script:found[0]
New-Item -ItemType Directory -Force $Out | Out-Null
function Shot($name) {
  $r = New-Object M+R; [M]::GetWindowRect($h,[ref]$r) | Out-Null
  $w = $r.Rt - $r.L; $ht = $r.B - $r.T
  $bmp = New-Object Drawing.Bitmap $w,$ht; $g = [Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
  [M]::PrintWindow($h,$dc,2) | Out-Null; $g.ReleaseHdc($dc)
  $bmp.Save("$Out\$name.png"); $g.Dispose(); $bmp.Dispose()
  "$name ${w}x${ht} at $($r.L),$($r.T)"
}
# Place the restored window on the chosen monitor.
$screens = [Windows.Forms.Screen]::AllScreens
$scr = if ($Monitor -eq 'main') { $screens | ? Primary } else { $screens | ? { -not $_.Primary } | Select -First 1 }
"monitor: $($scr.DeviceName) $($scr.Bounds)"
$SC_MAXIMIZE = 0xF030; $SC_RESTORE = 0xF120; $WM_SYSCOMMAND = 0x0112
[M]::SendMessage($h, $WM_SYSCOMMAND, [IntPtr]$SC_RESTORE, [IntPtr]::Zero) | Out-Null
Start-Sleep -Milliseconds 600
[M]::SetWindowPos($h, [IntPtr]::Zero, $scr.WorkingArea.X + 100, $scr.WorkingArea.Y + 80, 0, 0, 0x0001 -bor 0x0004) | Out-Null
[M]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 1500
Shot 'a0-restored'
$sw = [Diagnostics.Stopwatch]::StartNew()
[M]::SendMessage($h, $WM_SYSCOMMAND, [IntPtr]$SC_MAXIMIZE, [IntPtr]::Zero) | Out-Null
foreach ($t in 30, 100, 200, 350, 600, 1000, 1800) {
  while ($sw.ElapsedMilliseconds -lt $t) { Start-Sleep -Milliseconds 5 }
  Shot ("b{0:0000}-max" -f $t)
}
Start-Sleep -Milliseconds 800
$sw.Restart()
[M]::SendMessage($h, $WM_SYSCOMMAND, [IntPtr]$SC_RESTORE, [IntPtr]::Zero) | Out-Null
foreach ($t in 30, 150, 400, 1000) {
  while ($sw.ElapsedMilliseconds -lt $t) { Start-Sleep -Milliseconds 5 }
  Shot ("c{0:0000}-restore" -f $t)
}
