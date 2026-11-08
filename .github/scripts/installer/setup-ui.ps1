# Helpers for the installer test: find setup.exe's window and its controls through UI
# Automation, press buttons, and save pictures of the window. Dot-source it:
#
#   . .github/scripts/installer/setup-ui.ps1

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing

$script:Auto = [System.Windows.Automation.AutomationElement]
$script:Scope = [System.Windows.Automation.TreeScope]

# The automation id of the window the functions below work on: setup.exe's, unless a test
# sets another, such as the APK install window's (ApkInstallWindow).
$script:WindowId = "SetupWindow"

function Find-SetupWindow {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::AutomationIdProperty, $script:WindowId)
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

# A new UI Automation element for the window $Window stands for. UI Automation asks a window
# for its provider (WM_GETOBJECT) once, when it makes the element, and keeps the answer. An app
# still starting up can miss that question: its UI thread is busy, or its content is not
# attached yet. The element then stays a bare Win32 window (framework "Win32") with none of
# the app's controls under it, however long one waits on it. A new element asks again.
function Update-Window($Window) {
    # A window that has closed has no handle to ask again; the old element then says so itself.
    try {
        $Auto::FromHandle([IntPtr]$Window.Current.NativeWindowHandle)
    } catch {
        $Window
    }
}

# The top-level window of the process with this id, waiting for it to appear and for the app
# itself to answer UI Automation for it, which a window seen too early does not (Update-Window).
# A window that never answers is still returned at the deadline, so the caller's own check
# fails on it and says what is missing.
function Wait-ProcessWindow([int]$ProcessId, [int]$Seconds = 60) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::ProcessIdProperty, $ProcessId)
    $deadline = (Get-Date).AddSeconds($Seconds)
    $window = $null
    while ((Get-Date) -lt $deadline) {
        try {
            $window = if ($window) { Update-Window $window } else { $Auto::RootElement.FindFirst($Scope::Children, $condition) }
            if ($window -and $window.Current.FrameworkId -ne 'Win32') { return $window }
        } catch {
            # The window closed while it was being read: look for the process's window again.
            $window = $null
        }
        Start-Sleep -Milliseconds 500
    }
    if ($window) { Write-Host "the window of process $ProcessId still shows UI Automation only its Win32 frame after $Seconds seconds" }
    $window
}

# The label DSA Settings shows for $Page (general, memory, apps, ...) in $Lang (en-US when
# empty), read from the app's own string tables so the test and the app cannot disagree.
$script:SettingsPageKeys = @{
    general = 'NavGeneral'; memory = 'NavMemory'; apps = 'NavApps'; background = 'NavBackground'
    widgets = 'NavWidgets'; display = 'NavDisplay'; updates = 'NavUpdates'
    device = 'NavIdentity'; network = 'NavNetwork'; advanced = 'NavAdvanced'; logs = 'NavLogs'; about = 'NavAbout'
}
function Get-SettingsPageLabel([string]$Page, [string]$Lang = '') {
    $key = $SettingsPageKeys[$Page]
    if (-not $key) { throw "DSA Settings has no page called '$Page'" }
    $dir = Join-Path $PSScriptRoot '..\..\..\dsa\host\platform\windows\settings\DsaSettings'
    $file = if ($Lang -like 'ko*') { 'Strings.ko.resx' } else { 'Strings.resx' }
    [xml]$table = Get-Content -Raw -Encoding utf8 (Join-Path $dir $file)
    $value = ($table.root.data | Where-Object { $_.name -eq "$key.Content" } | Select-Object -First 1).value
    if (-not $value) { throw "$file has no label for the $Page page" }
    $value
}

# The navigation pane's item labelled $Label: a list item whose UI Automation name is its label
# and that supports selection.
function Find-SettingsPaneItem($Window, [string]$Label) {
    $condition = New-Object System.Windows.Automation.AndCondition(
        (New-Object System.Windows.Automation.PropertyCondition($Auto::NameProperty, $Label)),
        (New-Object System.Windows.Automation.PropertyCondition($Auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)),
        (New-Object System.Windows.Automation.PropertyCondition($Auto::IsSelectionItemPatternAvailableProperty, $true)))
    $Window.FindFirst($Scope::Descendants, $condition)
}

# Says why a pane item could not be found: whether the app still runs, what UI Automation
# shows of its window, every list item in it (from the raw view, which hides nothing) with
# whether it can be selected and where it is, and a picture of the screen named $Name.
function Show-PaneItems($Window, [string]$Name) {
    $fresh = Update-Window $Window
    try {
        $id = $fresh.Current.ProcessId
        $process = Get-Process -Id $id -ErrorAction SilentlyContinue
        $state = if ($process) { "runs" } else { "has exited" }
        Write-Host ("window '{0}' of process {1} ({2}): framework '{3}', the element the test held: framework '{4}'" -f
            $fresh.Current.Name, $id, $state, $fresh.Current.FrameworkId, $Window.Current.FrameworkId)
        $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
        $stack = New-Object System.Collections.Stack
        $stack.Push($fresh)
        $count = 0
        while ($stack.Count -gt 0) {
            $element = $stack.Pop()
            $child = $walker.GetFirstChild($element)
            while ($child) {
                if ($child.Current.ControlType -eq [System.Windows.Automation.ControlType]::ListItem) {
                    $count++
                    $selectable = [bool]$child.GetCurrentPropertyValue($Auto::IsSelectionItemPatternAvailableProperty)
                    Write-Host ("list item '{0}' class '{1}': selectable {2}, offscreen {3}, at {4}" -f
                        $child.Current.Name, $child.Current.ClassName, $selectable, $child.Current.IsOffscreen, $child.Current.BoundingRectangle)
                }
                $stack.Push($child)
                $child = $walker.GetNextSibling($child)
            }
        }
        Write-Host "list items in the window: $count"
    } catch {
        Write-Host "reading the window through UI Automation failed: $_"
    }
    Show-Desktop $Name
}

# Opens $Page in the DSA Settings $Window the way a screen reader does: it finds the pane item
# by its name and selects it through UI Automation, then waits until the item says it is selected.
function Open-SettingsPage($Window, [string]$Page, [string]$Lang = '', [int]$Seconds = 60) {
    $label = Get-SettingsPageLabel $Page $Lang
    $deadline = (Get-Date).AddSeconds($Seconds)
    $item = Find-SettingsPaneItem $Window $label
    while (-not $item -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        # A new element each time, so one made before the app answered cannot hide the pane.
        $item = Find-SettingsPaneItem (Update-Window $Window) $label
    }
    if (-not $item) {
        Show-PaneItems $Window "pane-missing-$Page"
        throw "the navigation pane has no selectable item named '$label' within $Seconds seconds"
    }
    $selection = $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
    $selection.Select()
    while (-not $selection.Current.IsSelected -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }
    if (-not $selection.Current.IsSelected) { throw "selecting '$label' through UI Automation did not open its page" }
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

# The text elements of $Window named exactly $Label.
function Find-RowTexts($Window, [string]$Label) {
    $condition = New-Object System.Windows.Automation.AndCondition(
        (New-Object System.Windows.Automation.PropertyCondition($Auto::NameProperty, $Label)),
        (New-Object System.Windows.Automation.PropertyCondition($Auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)))
    @($Window.FindAll($Scope::Descendants, $condition))
}

# Whether a row's name has room: a row squeezed to no width wraps its name at every letter, so
# the text is either empty or far taller than one line.
function Test-RowText($Text) {
    $r = $Text.Current.BoundingRectangle
    -not $r.IsEmpty -and $r.Width -ge 20 -and $r.Height -gt 0 -and $r.Height -le 60
}

# Whether $Element lies inside $Window. UI Automation's IsOffscreen stays false for a row
# scrolled out of a page's scroll viewer, so it cannot say this.
function Test-InWindow($Window, $Element) {
    $w = $Window.Current.BoundingRectangle
    $r = $Element.Current.BoundingRectangle
    -not $Element.Current.IsOffscreen -and $r.Top -ge $w.Top -and $r.Bottom -le $w.Bottom
}

# Fails unless every label is a laid out row of the list on the page in $Window, and at least
# $MinVisible of them are in view. A count above the list says nothing about this: the blank
# Apps page had the right count.
function Assert-RowsShown($Window, [string[]]$Labels, [int]$MinVisible = 1, [int]$Seconds = 30) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        $missing = @($Labels | Where-Object { -not (Find-RowTexts $Window $_ | Where-Object { Test-RowText $_ }) })
        $visible = @($Labels | Where-Object { Find-RowTexts $Window $_ | Where-Object { (Test-RowText $_) -and (Test-InWindow $Window $_) } })
        if ($missing.Count -eq 0 -and $visible.Count -ge $MinVisible) {
            Write-Host "rows laid out: $($Labels.Count), in view: $($visible.Count) ($($visible -join ', '))"
            return
        }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    $all = $Window.FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($Auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)))
    Write-Host "text on the page: $(($all | ForEach-Object { $_.Current.Name }) -join ' | ')"
    # The control view leaves out text a control draws in its own template, such as an error
    # bar's message, which says why a list stayed empty; the raw view has it.
    $raw = New-Object System.Windows.Automation.CacheRequest
    $raw.TreeFilter = [System.Windows.Automation.Automation]::RawViewCondition
    $raw.Add($Auto::NameProperty)
    $active = $raw.Activate()
    try {
        $texts = $Window.FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($Auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)))
        Write-Host "text on the page, raw view: $(($texts | ForEach-Object { $_.Cached.Name } | Where-Object { $_ }) -join ' | ')"
    } finally {
        $active.Dispose()
    }
    if (Test-Path screens) {
        Save-WindowScreen $Window (Join-Path $PWD "screens\rows-missing-$(Get-Date -Format HHmmss).png")
    }
    foreach ($label in $Labels) {
        foreach ($text in (Find-RowTexts $Window $label)) {
            Write-Host "'$label': $($text.Current.BoundingRectangle) offscreen=$($text.Current.IsOffscreen)"
        }
    }
    throw "the list shows no usable row for: $($missing -join ', '); $($visible.Count) rows in view, wanted $MinVisible"
}

# Scrolls the page's list to its end.
function Set-PageScrolledToEnd($Window) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($Auto::IsScrollPatternAvailableProperty, $true)
    foreach ($candidate in $Window.FindAll($Scope::Descendants, $condition)) {
        $scroll = $candidate.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
        if ($scroll.Current.VerticallyScrollable -and $candidate.Current.BoundingRectangle.X -gt ($Window.Current.BoundingRectangle.X + 200)) {
            $scroll.SetScrollPercent([System.Windows.Automation.ScrollPattern]::NoScroll, 100)
            Start-Sleep -Milliseconds 500
            return
        }
    }
    throw "the page has nothing that scrolls"
}

# The control of the row named $Label with this control type and, if given, this name.
function Find-RowControl($Window, [string]$Label, $ControlType, [string]$Name) {
    $condition = New-Object System.Windows.Automation.AndCondition(
        (New-Object System.Windows.Automation.PropertyCondition($Auto::HelpTextProperty, $Label)),
        (New-Object System.Windows.Automation.PropertyCondition($Auto::ControlTypeProperty, $ControlType)))
    $found = @($Window.FindAll($Scope::Descendants, $condition) | Where-Object { -not $Name -or $_.Current.Name -eq $Name })
    if ($found.Count -eq 0) { throw "the row '$Label' has no $($ControlType.ProgrammaticName) '$Name'" }
    $found[0]
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

# Some runner images open Windows' first sign-in privacy screen over the whole desktop, which
# takes the mouse wheel and the foreground from the window under test. It is closed and kept
# from coming back.
function Close-PrivacyExperience {
    $key = 'HKCU:\Software\Policies\Microsoft\Windows\OOBE'
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name DisablePrivacyExperience -Value 1 -Type DWord
    Get-Process UserOOBEBroker, WWAHost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}
