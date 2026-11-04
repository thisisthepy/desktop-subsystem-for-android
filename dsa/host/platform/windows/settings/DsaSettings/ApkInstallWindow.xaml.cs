using System.ComponentModel;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Shapes;
using Dsa.Settings.Core;
using Palette = Dsa.Setup.Palette;

namespace Dsa.Settings;

/// <summary>
/// The window an APK opens in: what the file says about the app, then its install. It follows
/// the theme Windows uses for apps, as setup does, and changes with it.
/// </summary>
public partial class ApkInstallWindow : Window
{
    private const int WmSettingChange = 0x001A;

    private readonly ApkInstallViewModel _vm;
    private bool _dark;

    private ApkInstallWindow(string path)
    {
        _vm = new ApkInstallViewModel(
            new ProcessDsaRunner(ProcessDsaCli.Locate(AppContext.BaseDirectory)), App.Text, path);
        InitializeComponent();
        _dark = Palette.IsDark();
        Palette.Apply(this, _dark);
        if (!Palette.HasMica)
        {
            // Without Mica a full glass frame would show black; a 1 px edge keeps the shadow.
            Chrome.GlassFrameThickness = new Thickness(0, 0, 0, 1);
        }
        Icon = BitmapFrame.Create(new Uri("pack://application:,,,/Assets/logo.ico"));
        Resources["SensitiveText"] = App.Text.Get("ApkWindow_Sensitive");
        Title = App.Text.Get("ApkWindow_Caption");
        CaptionTitle.Text = Title;
        TrademarkText.Text = App.Text.Get("AboutTrademark.Text");
        ReadingText.Text = App.Text.Get("ApkWindow_Reading");
        CancelButton.Content = App.Text.Get("ApkWindow_Cancel");
        CloseButton.Content = App.Text.Get("ApkWindow_Close");
        OpenButton.Content = App.Text.Get("ApkWindow_Open");
        System.Windows.Automation.AutomationProperties.SetName(CaptionClose, App.Text.Get("ApkWindow_Close"));
        Marquee(ReadingBar);
        Marquee(ProgressMarquee);
        _vm.PropertyChanged += (_, _) => Render();
        Render();
        SourceInitialized += (_, _) =>
        {
            var handle = new WindowInteropHelper(this).Handle;
            Palette.ApplyBackdrop(handle, _dark);
            HwndSource.FromHwnd(handle)?.AddHook(Hook);
        };
        Loaded += async (_, _) => await _vm.LoadAsync();
        DragOver += (_, e) =>
        {
            e.Effects = Files(e).Any(InstallViewModel.IsApk) ? DragDropEffects.Copy : DragDropEffects.None;
            e.Handled = true;
        };
        Drop += (_, e) =>
        {
            foreach (var apk in Files(e).Where(InstallViewModel.IsApk))
            {
                Open(apk);
            }
        };
    }

    /// <summary>Opens a window for the APK at <paramref name="path"/>.</summary>
    public static ApkInstallWindow Open(string path)
    {
        var window = new ApkInstallWindow(path);
        window.Show();
        window.Activate();
        return window;
    }

    private static IEnumerable<string> Files(DragEventArgs e) =>
        e.Data.GetDataPresent(DataFormats.FileDrop) && e.Data.GetData(DataFormats.FileDrop) is string[] files
            ? files
            : Enumerable.Empty<string>();

    /// <summary>Follows a change of the apps' theme while the window is open.</summary>
    private IntPtr Hook(IntPtr hwnd, int msg, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (msg == WmSettingChange)
        {
            var dark = Palette.IsDark();
            if (dark != _dark)
            {
                _dark = dark;
                Palette.Apply(this, dark);
            }
        }
        return IntPtr.Zero;
    }

    /// <summary>Fills a 3 px strip with an indicator that slides across it, for a step that
    /// has no measure, in the progress bar's colours.</summary>
    private static void Marquee(Grid strip)
    {
        var track = new Border { Height = 1, VerticalAlignment = VerticalAlignment.Center };
        track.SetResourceReference(Border.BackgroundProperty, "Track");
        var indicator = new Border
        {
            Width = 120,
            Height = 3,
            CornerRadius = new CornerRadius(1.5),
            HorizontalAlignment = HorizontalAlignment.Left,
            RenderTransform = new TranslateTransform(-120, 0),
        };
        indicator.SetResourceReference(Border.BackgroundProperty, "Accent");
        strip.Children.Add(track);
        strip.Children.Add(indicator);
        strip.SizeChanged += (_, _) =>
        {
            var slide = new DoubleAnimation(-120, Math.Max(strip.ActualWidth, 1), TimeSpan.FromSeconds(1.6))
            {
                RepeatBehavior = RepeatBehavior.Forever,
                EasingFunction = new SineEase { EasingMode = EasingMode.EaseInOut },
            };
            indicator.RenderTransform.BeginAnimation(TranslateTransform.XProperty, slide);
        };
    }

    private static Visibility Visible(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;

    private void Render()
    {
        var page = _vm.Page;
        HeroTitle.Text = _vm.Title;
        HeroPackage.Text = _vm.Subtitle;
        Chips.Visibility = Visible(_vm.Apk is not null);
        VersionChip.Text = _vm.VersionChip;
        SizeChip.Text = _vm.SizeChip;
        AndroidChip.Text = _vm.AndroidChip;
        AppIcon.Source = IconImage(_vm.Apk?.Icon);

        ReadingPanel.Visibility = Visible(page == ApkInstallPage.Reading);
        DetailsPanel.Visibility = Visible(page == ApkInstallPage.Details);
        ProgressPanel.Visibility = Visible(page == ApkInstallPage.Progress);
        ResultPanel.Visibility = Visible(page is ApkInstallPage.Done or ApkInstallPage.Failed);

        SummaryText.Text = _vm.Summary;
        SummaryBadge.Visibility = Visible(_vm.IsBlocked);
        FactList.ItemsSource = _vm.Facts;
        PermissionsHeader.Text = _vm.PermissionsHeader;
        PermissionList.ItemsSource = _vm.Permissions;

        ProgressTitle.Text = _vm.ProgressTitle;
        ProgressDetail.Text = _vm.ProgressDetail;
        Progress.Visibility = Visible(_vm.ProgressPercent is not null);
        ProgressMarquee.Visibility = Visible(_vm.ProgressPercent is null);
        Progress.Value = _vm.ProgressPercent ?? 0;
        ProgressPercent.Text = _vm.ProgressPercent is { } percent ? percent + "%" : "";

        var good = page == ApkInstallPage.Done;
        ResultBadge.SetResourceReference(Shape.FillProperty, good ? "Accent" : "Critical");
        ResultGlyph.SetResourceReference(Shape.StrokeProperty, good ? "AccentText" : "CriticalText");
        ResultGlyph.Data = Geometry.Parse(good ? "M3.5,8.5 L6.5,11.5 L12.5,4.5" : "M5.5,5.5 L10.5,10.5 M10.5,5.5 L5.5,10.5");
        ResultTitle.Text = _vm.ResultTitle;
        ResultText.Text = _vm.ResultText;

        InstallButton.Content = _vm.InstallButton;
        InstallButton.Visibility = Visible(page is ApkInstallPage.Reading or ApkInstallPage.Details);
        InstallButton.IsEnabled = _vm.CanInstall;
        CancelButton.Visibility = Visible(page is ApkInstallPage.Reading or ApkInstallPage.Details or ApkInstallPage.Progress);
        CancelButton.IsEnabled = page != ApkInstallPage.Progress || _vm.CanCancel;
        OpenButton.Visibility = Visible(_vm.CanOpen);
        CloseButton.Visibility = Visible(page is ApkInstallPage.Done or ApkInstallPage.Failed);

        // Enter presses the screen's main button; Escape its way out.
        InstallButton.IsDefault = _vm.CanInstall;
        OpenButton.IsDefault = _vm.CanOpen;
        CloseButton.IsDefault = page == ApkInstallPage.Failed;
        CancelButton.IsCancel = CancelButton.Visibility == Visibility.Visible;
        CloseButton.IsCancel = !CancelButton.IsCancel;
    }

    /// <summary>The app's icon at 72 px: its image, or its adaptive icon's layers masked to a
    /// rounded square as a launcher shows them. Without one, DSA's own logo.</summary>
    private static ImageSource IconImage(ApkIcon? icon)
    {
        try
        {
            if (icon?.Png is { } png)
            {
                return Bitmap(png);
            }
            if (icon is { IsAdaptive: true })
            {
                // Each layer is 108 dp; the middle 72 dp show.
                var full = new Rect(0, 0, 108, 108);
                var group = new DrawingGroup { ClipGeometry = new RectangleGeometry(new Rect(18, 18, 72, 72), 16, 16) };
                foreach (var layer in new[] { icon.Background, icon.Foreground })
                {
                    if (layer?.Color is { } argb)
                    {
                        var color = Color.FromArgb((byte)(argb >> 24), (byte)(argb >> 16), (byte)(argb >> 8), (byte)argb);
                        group.Children.Add(new GeometryDrawing(new SolidColorBrush(color), null, new RectangleGeometry(full)));
                    }
                    else if (layer?.Png is { } image)
                    {
                        group.Children.Add(new ImageDrawing(Bitmap(image), full));
                    }
                    else if (layer == icon.Background)
                    {
                        group.Children.Add(new GeometryDrawing(Brushes.White, null, new RectangleGeometry(full)));
                    }
                }
                var drawn = new DrawingImage(group);
                drawn.Freeze();
                return drawn;
            }
        }
        catch (Exception e) when (e is NotSupportedException or IOException or ArgumentException or FileFormatException)
        {
            // An image Windows cannot decode shows as DSA's logo.
        }
        return new BitmapImage(new Uri("pack://application:,,,/Assets/logo.png"));
    }

    private static BitmapImage Bitmap(byte[] data)
    {
        var image = new BitmapImage();
        image.BeginInit();
        image.CacheOption = BitmapCacheOption.OnLoad;
        image.StreamSource = new MemoryStream(data);
        image.EndInit();
        image.Freeze();
        return image;
    }

    private async void OnInstall(object sender, RoutedEventArgs e) => await _vm.InstallAsync();

    private async void OnOpen(object sender, RoutedEventArgs e)
    {
        OpenButton.IsEnabled = false;
        var why = await _vm.OpenAsync();
        OpenButton.IsEnabled = true;
        if (why is null)
        {
            Close();
        }
        else
        {
            ResultText.Text = why;
        }
    }

    private void OnCancel(object sender, RoutedEventArgs e)
    {
        if (_vm.IsBusy)
        {
            _vm.Cancel();
        }
        else
        {
            Close();
        }
    }

    private void OnClose(object sender, RoutedEventArgs e) => Close();

    private void OnCaptionClose(object sender, RoutedEventArgs e) => Close();

    private void OnCaptionMinimize(object sender, RoutedEventArgs e) => WindowState = WindowState.Minimized;

    protected override void OnClosing(CancelEventArgs e)
    {
        // Closing during an install stops it, as Cancel does; the window closes once it has.
        if (_vm.IsBusy)
        {
            _vm.Cancel();
        }
        base.OnClosing(e);
    }
}
