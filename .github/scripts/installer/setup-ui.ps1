# Helpers for the installer test: find setup.exe's window and its controls through UI
# Automation, press buttons, and save pictures of the window. Dot-source it:
#
#   . .github/scripts/installer/setup-ui.ps1

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing

$script:Auto = [System.Windows.Automation.AutomationElement]
$script:Scope = [System.Windows.Automation.TreeScope]

function Find-SetupWindow {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::AutomationIdProperty, "SetupWindow")
    $Auto::RootElement.FindFirst($Scope::Children, $condition)
}

# Waits until the setup window shows the control with this automation id (and, with
# -Enabled, until it can be pressed). Returns the control.
function Wait-SetupControl([string]$Id, [int]$Seconds = 60, [switch]$Enabled) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::AutomationIdProperty, $Id)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $window = Find-SetupWindow
        if ($window) {
            $control = $window.FindFirst($Scope::Descendants, $condition)
            if ($control -and -not $control.Current.IsOffscreen -and (-not $Enabled -or $control.Current.IsEnabled)) {
                return $control
            }
        }
        Start-Sleep -Milliseconds 250
    }
    Show-Desktop "timeout-$Id"
    throw "setup.exe did not show $Id within $Seconds seconds"
}

# Says what is on the screen: the top-level windows and a picture of the whole screen.
function Show-Desktop([string]$Name) {
    $Auto::RootElement.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object {
        $process = Get-Process -Id $_.Current.ProcessId -ErrorAction SilentlyContinue
        Write-Host ("window '{0}' id '{1}' process {2}" -f $_.Current.Name, $_.Current.AutomationId, $process.ProcessName)
    }
    Get-Process | Where-Object { $_.ProcessName -match 'setup|DsaSetup' } | ForEach-Object { Write-Host "process $($_.ProcessName) $($_.Id)" }
    New-Item -ItemType Directory -Force screens | Out-Null
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap $screen.Width, $screen.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($screen.Location, [System.Drawing.Point]::Empty, $screen.Size)
    $bitmap.Save("$PWD\screens\$Name.png")
    $graphics.Dispose()
    $bitmap.Dispose()
}

function Invoke-SetupButton([string]$Id, [int]$Seconds = 60) {
    $button = Wait-SetupControl $Id -Seconds $Seconds -Enabled
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}

function Get-SetupText([string]$Id) {
    (Wait-SetupControl $Id).Current.Name
}

# Saves a picture of the setup window and 32 px around it (the shadow and rounded corners).
function Save-SetupScreen([string]$Path) {
    $window = Find-SetupWindow
    if (-not $window) { throw "no setup window to take a picture of" }
    Save-WindowScreen $window $Path
}

# The top-level window of the process with this id, waiting for it to appear.
function Wait-ProcessWindow([int]$ProcessId, [int]$Seconds = 60) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::ProcessIdProperty, $ProcessId)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $window = $Auto::RootElement.FindFirst($Scope::Children, $condition)
        if ($window) { return $window }
        Start-Sleep -Milliseconds 500
    }
    $null
}

# Selects the item named $Name inside $Window (such as a navigation item). Returns whether it
# could.
function Select-Named($Window, [string]$Name) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::NameProperty, $Name)
    $item = $Window.FindFirst($Scope::Descendants, $condition)
    if (-not $item) { return $false }
    $pattern = $null
    if ($item.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
        $pattern.Select()
        return $true
    }
    if ($item.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
        $pattern.Invoke()
        return $true
    }
    $false
}

# Saves a picture of $Window and 32 px around it.
function Save-WindowScreen($Window, [string]$Path) {
    Start-Sleep -Milliseconds 600   # let the window finish drawing
    $r = $Window.Current.BoundingRectangle
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $x = [Math]::Max(0, [int]$r.X - 32)
    $y = [Math]::Max(0, [int]$r.Y - 32)
    $w = [Math]::Min($screen.Width - $x, [int]$r.Width + 64)
    $h = [Math]::Min($screen.Height - $y, [int]$r.Height + 64)
    $bitmap = New-Object System.Drawing.Bitmap $w, $h
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $w, $h))
    $bitmap.Save($Path)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "saved $Path"
}

# Apps use the light (1) or the dark (0) theme. setup.exe reads it when it starts.
function Set-AppTheme([switch]$Dark) {
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
    if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
    Set-ItemProperty -Path $key -Name AppsUseLightTheme -Value ([int](-not $Dark)) -Type DWord
}

function Wait-SetupExit($Process, [int]$Seconds = 120) {
    if (-not $Process.WaitForExit($Seconds * 1000)) { throw "setup.exe did not finish within $Seconds seconds" }
    $Process.ExitCode
}
