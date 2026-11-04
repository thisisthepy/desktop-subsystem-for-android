# A stand-in for dsa.exe for the pictures of the settings app's Widgets page. The installer
# test has no real Android image, so nothing in it offers a widget; this answers
# `dsa widgets list --json` with three widgets (one with a preview, one that asks to be set up,
# one with no picture at all, which shows the stand-in icon) and one placed widget, answers
# `dsa widgets watch --json` with the placed widget and then waits, and hands every other
# command to the real dsa.exe. Dot-source it, then:
#
#   $fake = New-WidgetsFixture "$env:RUNNER_TEMP\widgets" "$env:PROGRAM_DIR\dsa.exe"
#   $env:DSA_CLI = $fake

Add-Type -AssemblyName System.Drawing

# A PNG picture of a widget, `$Width` by `$Height` pixels, as base64.
function New-WidgetPreview([int]$Width, [int]$Height, [string]$Color, [string]$Text) {
    $bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($Color))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = 24
    $path.AddArc(0, 0, $r, $r, 180, 90)
    $path.AddArc($Width - $r - 1, 0, $r, $r, 270, 90)
    $path.AddArc($Width - $r - 1, $Height - $r - 1, $r, $r, 0, 90)
    $path.AddArc(0, $Height - $r - 1, $r, $r, 90, 90)
    $path.CloseFigure()
    $graphics.FillPath($brush, $path)
    $font = New-Object System.Drawing.Font 'Segoe UI', 28, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Center
    $graphics.DrawString($Text, $font, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF 0, 0, $Width, $Height), $format)
    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    [Convert]::ToBase64String($stream.ToArray())
}

# Writes the stand-in to `$Dir` and returns the path of the command to run as dsa.
function New-WidgetsFixture([string]$Dir, [string]$RealDsa) {
    New-Item -ItemType Directory -Force $Dir | Out-Null
    $clock = New-WidgetPreview 160 96 '#2F6FDE' '10:42'
    $weather = New-WidgetPreview 160 160 '#E8833A' '21 C'
    $placed = [ordered]@{
        id = 3; provider = 'com.example.clock/.ClockWidget'; package = 'com.example.clock'
        label = 'Clock'; app = 'Clock'
        placement = [ordered]@{ x = 64; y = 64; width_dp = 180; height_dp = 110 }
    }
    $list = [ordered]@{
        providers = @(
            [ordered]@{ provider = 'com.example.clock/.ClockWidget'; package = 'com.example.clock'; label = 'Clock'
                app = 'Clock'; preview_png = $clock; default_width_dp = 180; default_height_dp = 110; needs_configuration = $false }
            [ordered]@{ provider = 'com.example.notes/.NoteWidget'; package = 'com.example.notes'; label = 'Sticky note'
                app = 'Notes'; preview_png = ''; default_width_dp = 250; default_height_dp = 250; needs_configuration = $false }
            [ordered]@{ provider = 'com.example.weather/.Forecast'; package = 'com.example.weather'; label = 'Forecast'
                app = 'Weather'; preview_png = $weather; default_width_dp = 250; default_height_dp = 180; needs_configuration = $true }
        )
        widgets = @($placed)
    }
    $list | ConvertTo-Json -Depth 5 -Compress | Set-Content -Encoding ascii "$Dir\widgets.json"
    [ordered]@{ event = 'widgets'; widgets = @($placed) } | ConvertTo-Json -Depth 5 -Compress |
        Set-Content -Encoding ascii "$Dir\watch.json"
    $cmd = @"
@echo off
if "%~1 %~2"=="widgets list" (
  type "%~dp0widgets.json"
  exit /b 0
)
if "%~1 %~2"=="widgets watch" (
  type "%~dp0watch.json"
  ping -n 600 127.0.0.1 >nul
  exit /b 0
)
"$RealDsa" %*
"@
    Set-Content -Encoding ascii "$Dir\dsa.cmd" $cmd
    "$Dir\dsa.cmd"
}
