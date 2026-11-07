using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class AdvancedPage : Page
{
    /// <summary>Reads where adb auto-connect stands while the page is shown.</summary>
    private readonly DispatcherTimer _adbTimer = new() { Interval = TimeSpan.FromSeconds(3) };

    private static SettingsViewModel ViewModel => App.Settings;

    public AdvancedPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        _adbTimer.Tick += async (_, _) => await ViewModel.PollAdbAutoConnectAsync();
        Loaded += async (_, _) =>
        {
            _adbTimer.Start();
            await ViewModel.LoadAsync();
        };
        Unloaded += (_, _) => _adbTimer.Stop();
    }

    private async void OnLinksToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetLinksInAndroidAsync(LinksSwitch.IsChecked == true);

    private async void OnClipboardToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetClipboardAsync(ClipboardSwitch.IsChecked == true);

    private async void OnFileSharingToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFileSharingAsync(FileSharingSwitch.IsChecked == true);

    private async void OnFolderToggled(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox { Tag: string folder } box)
        {
            await ViewModel.SetFolderSharedAsync(folder, box.IsChecked == true);
        }
    }

    private async void OnOpenAndroidFiles(object sender, RoutedEventArgs e) =>
        await ViewModel.OpenAndroidFilesAsync();

    private async void OnArmTranslationToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetArmTranslationAsync(ArmTranslationSwitch.IsChecked == true);

    private async void OnRemoveArmTranslation(object sender, RoutedEventArgs e) =>
        await ViewModel.RemoveArmTranslationAsync();

    private async void OnUseDefaultArmTranslator(object sender, RoutedEventArgs e) =>
        await ViewModel.UseDefaultArmTranslatorAsync();

    /// <summary>Says where Google's translator comes from and under which terms, and fetches it
    /// only when the user accepts.</summary>
    private async void OnFetchGoogleArm(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("ArmGoogleConfirm.Title"), ViewModel.GoogleArmNotice,
                App.Text.Get("ArmGoogleConfirm.Action")))
        {
            await ViewModel.FetchGoogleArmTranslatorAsync();
        }
    }

    private void OnOpenGoogleArmTerms(object sender, RoutedEventArgs e) => OpenWebPage(ViewModel.GoogleArmTermsUrl);

    private async void OnInstallGapps(object sender, RoutedEventArgs e) =>
        await ViewModel.InstallGappsAsync();

    private async void OnRemoveGapps(object sender, RoutedEventArgs e) =>
        await ViewModel.RemoveGappsAsync();

    private void OnOpenGappsTerms(object sender, RoutedEventArgs e) => OpenWebPage(ViewModel.GappsTermsUrl);

    private void OnOpenGappsRegistration(object sender, RoutedEventArgs e) =>
        OpenWebPage(ViewModel.GappsRegistrationUrl);

    /// <summary>Opens an https page in the default browser. The view model gives only https
    /// addresses; anything else is left alone.</summary>
    private static void OpenWebPage(string url)
    {
        if (!url.StartsWith("https://", System.StringComparison.OrdinalIgnoreCase))
        {
            return;
        }
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(url) { UseShellExecute = true });
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // No program opens web pages on this computer, so nothing opens.
        }
    }

    /// <summary>Copies the Android ID the registration page asks for, and shows a check mark
    /// on the button for a moment.</summary>
    private async void OnCopyGappsDeviceId(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(GappsDeviceIdBox.Text, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        GappsDeviceIdCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        GappsDeviceIdCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    private async void OnDeveloperModeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetDeveloperModeAsync(DeveloperModeSwitch.IsChecked == true);

    private async void OnAdbAutoConnectToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdbAutoConnectAsync(AdbAutoConnectSwitch.IsChecked == true);

    /// <summary>Copies the adb command, and shows a check mark on the button for a moment. The
    /// clipboard can be held by another program, and then the button stays as it was.</summary>
    private async void OnCopyAdbCommand(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(AdbCommand.Text, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        AdbCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        AdbCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    private async void OnAdbPortChanged(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdbPortAsync(AdbPortBox.Text);

    private async void OnAdbPortKeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.Enter)
        {
            await ViewModel.SetAdbPortAsync(AdbPortBox.Text);
        }
    }

    /// <summary>Asks first, saying what is lost, then resets Android's data.</summary>
    private async void OnResetData(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("ResetDataConfirm.Title"), ViewModel.ResetWarning,
                App.Text.Get("ResetDataConfirm.Action")))
        {
            await ViewModel.ResetDataAsync();
        }
    }

    private async void OnFreezeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFreezeAsync(FreezeSwitch.IsChecked == true);

    private async void OnInputTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetInputTraceAsync(InputTraceSwitch.IsChecked == true);

    private async void OnResizeTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetResizeTraceAsync(ResizeTraceSwitch.IsChecked == true);

    private void OnInputTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("input-trace", !ViewModel.InputTrace);

    private void OnResizeTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("resize-trace", !ViewModel.ResizeTrace);

    /// <summary>Runs <c>dsa settings set</c> as an administrator (Windows asks the user),
    /// then shows what the service keeps.</summary>
    private static async void ChangeAsAdministrator(string name, bool on)
    {
        var problem = await Shell.RunDsaElevated("settings", "set", name, on ? "on" : "off");
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await ViewModel.LoadAsync();
    }

    private void OnOpenUserLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.ServiceLogFolder(ViewModel.LogDir));
}
