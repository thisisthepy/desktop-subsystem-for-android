using System.ComponentModel;
using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Dsa.Settings.Core;
using Microsoft.Win32;
using Wpf.Ui.Appearance;
using Wpf.Ui.Controls;

namespace Dsa.ApkInstaller;

/// <summary>
/// The window an APK opens in: what the file says about the app, then its install. It follows
/// the theme Windows uses for apps, with Mica behind it on Windows 11, and changes with it.
/// </summary>
public partial class InstallerWindow : FluentWindow
{
    private readonly ApkInstallViewModel _vm;
    private bool _closed;
    private ApkIcon? _drawnIcon;
    private bool _iconShown;

    private InstallerWindow(string path)
    {
        _vm = new ApkInstallViewModel(
            new ProcessDsaRunner(ProcessDsaCli.Locate(AppContext.BaseDirectory)), App.Text, path);
        InitializeComponent();
        Icon = BitmapFrame.Create(new Uri("pack://application:,,,/Assets/apk-installer.ico"));
        Resources["SensitiveText"] = App.Text.Get("ApkWindow_Sensitive");
        Title = ApkInstallerTitle.For(CultureInfo.CurrentUICulture);
        TitleBar.Title = Title;
        TrademarkText.Text = App.Text.Get("AboutTrademark.Text");
        ReadingText.Text = App.Text.Get("ApkWindow_Reading");
        PackageLabel.Text = ApkInstallViewModel.PackageLabel;
        PublisherLabel.Text = ApkInstallViewModel.PublisherLabel;
        VersionLabel.Text = ApkInstallViewModel.VersionLabel;
        SourceLabel.Text = ApkInstallViewModel.SourceLabel;
        LaunchWhenReady.Content = App.Text.Get("ApkWindow_LaunchWhenReady");
        LaunchWhenReady.IsChecked = _vm.LaunchWhenReady;
        LaunchWhenReady.Checked += (_, _) => _vm.LaunchWhenReady = true;
        LaunchWhenReady.Unchecked += (_, _) => _vm.LaunchWhenReady = false;
        CancelButton.Content = App.Text.Get("ApkWindow_Cancel");
        CloseButton.Content = App.Text.Get("ApkWindow_Close");
        OpenButton.Content = App.Text.Get("ApkWindow_Open");
        // The light or the dark theme Windows uses for apps, its accent colour, and their
        // changes while the window is open.
        SystemThemeWatcher.Watch(this, WindowBackdropType.Mica, true);
        _vm.PropertyChanged += (_, _) => Render();
        Render();
        Loaded += async (_, _) =>
        {
            // The watcher follows changes; the theme to start with is the apps' setting, which
            // it does not read itself.
            ApplicationThemeManager.Apply(AppsUseDarkTheme() ? ApplicationTheme.Dark : ApplicationTheme.Light,
                WindowBackdropType.Mica, true);
            await _vm.LoadAsync();
        };
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
    public static InstallerWindow Open(string path)
    {
        var window = new InstallerWindow(path);
        window.Show();
        window.Activate();
        return window;
    }

    private static bool AppsUseDarkTheme()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return key?.GetValue("AppsUseLightTheme") is int light && light == 0;
        }
        catch (Exception e) when (e is System.Security.SecurityException or UnauthorizedAccessException or IOException)
        {
            return false;
        }
    }

    private static IEnumerable<string> Files(DragEventArgs e) =>
        e.Data.GetDataPresent(DataFormats.FileDrop) && e.Data.GetData(DataFormats.FileDrop) is string[] files
            ? files
            : Enumerable.Empty<string>();

    private static Visibility Visible(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;

    private void Render()
    {
        if (_closed)
        {
            return;
        }
        if (_vm.Launched)
        {
            // The app opened by itself, as "Launch when ready" asked: the installer is done.
            _closed = true;
            Close();
            return;
        }
        var page = _vm.Page;
        var read = _vm.Apk is not null;
        HeroTitle.Text = _vm.HeroTitle;
        // The labels and the values in their two columns; the row goes with its value.
        HeroPackage.Text = _vm.PackageValue;
        PublisherText.Text = _vm.PublisherValue;
        PublisherText.ToolTip = _vm.PublisherLine;
        VersionChip.Text = _vm.VersionValue;
        SourceText.Text = _vm.SourceValue;
        SourceText.ToolTip = _vm.SourceLine;
        DetailRows.Visibility = Visible(read);
        // A screen reader says each line whole, its label with its value.
        System.Windows.Automation.AutomationProperties.SetName(HeroPackage, _vm.PackageLine);
        System.Windows.Automation.AutomationProperties.SetName(PublisherText, _vm.PublisherLine);
        System.Windows.Automation.AutomationProperties.SetName(VersionChip, _vm.VersionLine);
        System.Windows.Automation.AutomationProperties.SetName(SourceText, _vm.SourceLine);
        // The icon is drawn once for each icon the file gives; one that cannot be drawn, or
        // none, never takes the place of one already shown.
        if (!ReferenceEquals(_vm.Apk?.Icon, _drawnIcon))
        {
            _drawnIcon = _vm.Apk?.Icon;
            if (IconImage(_drawnIcon) is { } drawn)
            {
                _iconShown = true;
                AppIcon.Source = drawn;
            }
        }
        if (!_iconShown)
        {
            AppIcon.Source = InstallerIcon;
        }

        var showDetails = page is ApkInstallPage.Details or ApkInstallPage.Progress;
        SummaryPanel.Visibility = Visible(showDetails);
        SummaryBox.Visibility = Visible(_vm.Summary.Length > 0);
        SummaryTitle.Text = _vm.SummaryTitle;
        SummaryText.Text = _vm.Summary;
        // Android refuses what is blocked; the rest is for the user to know.
        SummaryBadge.Symbol = _vm.IsBlocked ? SymbolRegular.ErrorCircle24 : SymbolRegular.Info24;
        SummaryBadge.SetResourceReference(IconElement.ForegroundProperty,
            _vm.IsBlocked ? "SystemFillColorCriticalBrush" : "SystemFillColorAttentionBrush");
        SummaryBox.SetResourceReference(System.Windows.Controls.Border.BackgroundProperty,
            _vm.IsBlocked ? "InfoBarErrorSeverityBackgroundBrush" : "InfoBarInformationalSeverityBackgroundBrush");
        CompatibilityBox.Visibility = Visible(_vm.CompatibilityWarning.Length > 0);
        CompatibilityTitle.Text = _vm.CompatibilityTitle;
        CompatibilityText.Text = _vm.CompatibilityWarning;

        ReadingPanel.Visibility = Visible(page == ApkInstallPage.Reading);
        DetailsPanel.Visibility = Visible(showDetails);
        ResultPanel.Visibility = Visible(page is ApkInstallPage.Done or ApkInstallPage.Failed);

        FactList.ItemsSource = _vm.Facts;
        PermissionsHeader.Text = _vm.PermissionsHeader;
        PermissionPreview.ItemsSource = _vm.PermissionPreview;
        PermissionList.ItemsSource = _vm.Permissions;
        PermissionPreview.Visibility = Visible(!_vm.IsExpanded);
        PermissionList.Visibility = Visible(_vm.IsExpanded);
        ExpandedPanel.Visibility = Visible(_vm.IsExpanded);
        MoreLink.Content = _vm.MoreLabel;
        MoreLink.IsExpanded = _vm.IsExpanded;

        LaunchWhenReady.Visibility = Visible(page is ApkInstallPage.Reading or ApkInstallPage.Details);
        LaunchWhenReady.IsEnabled = !_vm.IsBlocked;
        ProgressPanel.Visibility = Visible(page == ApkInstallPage.Progress);
        ProgressTitle.Text = _vm.ProgressTitle;
        ProgressDetail.Text = _vm.ProgressDetail;
        Progress.IsIndeterminate = _vm.ProgressPercent is null;
        Progress.Value = _vm.ProgressPercent ?? 0;
        ProgressPercent.Text = _vm.ProgressPercent is { } percent ? percent + "%" : "";

        var good = page == ApkInstallPage.Done;
        ResultGlyph.Symbol = good ? SymbolRegular.CheckmarkCircle24 : SymbolRegular.ErrorCircle24;
        ResultGlyph.SetResourceReference(IconElement.ForegroundProperty,
            good ? "SystemFillColorSuccessBrush" : "SystemFillColorCriticalBrush");
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

    /// <summary>The app's icon at the tile's size: its image, its vector drawable, or its
    /// adaptive icon's layers masked to a rounded square as a launcher shows them. Null when
    /// the icon cannot be drawn.</summary>
    private static ImageSource? IconImage(ApkIcon? icon)
    {
        try
        {
            if (icon?.Png is { } png)
            {
                return Bitmap(png);
            }
            if (icon?.Vector is { } whole)
            {
                // A clear square of the full size keeps the viewport, not the paths, as the image.
                return VectorDrawing(whole, new Rect(0, 0, 108, 108)) is { } drawing
                    ? Frozen(new DrawingGroup { Children = { new GeometryDrawing(Brushes.Transparent, null, new RectangleGeometry(new Rect(0, 0, 108, 108))), drawing } })
                    : null;
            }
            if (icon is { IsAdaptive: true })
            {
                // Each layer is 108 dp; the middle 72 dp show.
                var full = new Rect(0, 0, 108, 108);
                var group = new DrawingGroup { ClipGeometry = new RectangleGeometry(new Rect(18, 18, 72, 72), 16, 16) };
                var drawn = false;
                foreach (var layer in new[] { icon.Background, icon.Foreground })
                {
                    if (layer?.Color is { } argb)
                    {
                        group.Children.Add(new GeometryDrawing(new SolidColorBrush(Argb(argb)), null, new RectangleGeometry(full)));
                        drawn = true;
                    }
                    else if (layer?.Png is { } image)
                    {
                        group.Children.Add(new ImageDrawing(Bitmap(image), full));
                        drawn = true;
                    }
                    else if (layer?.Vector is { } vector && VectorDrawing(vector, full) is { } drawing)
                    {
                        group.Children.Add(drawing);
                        drawn = true;
                    }
                    else if (ReferenceEquals(layer, icon.Background))
                    {
                        // A background the window cannot draw shows white, as a launcher's
                        // default does; the foreground is still drawn on it.
                        group.Children.Add(new GeometryDrawing(Brushes.White, null, new RectangleGeometry(full)));
                    }
                }
                // A white square alone is not the app's icon.
                return drawn ? Frozen(group) : null;
            }
        }
        catch (Exception e) when (e is NotSupportedException or IOException or ArgumentException or FileFormatException
            or InvalidOperationException)
        {
            // An image Windows cannot decode is not drawn.
        }
        return null;
    }

    private static readonly ImageSource InstallerIcon =
        new BitmapImage(new Uri("pack://application:,,,/Assets/apk-installer.png"));

    private static Color Argb(uint argb) =>
        Color.FromArgb((byte)(argb >> 24), (byte)(argb >> 16), (byte)(argb >> 8), (byte)argb);

    private static DrawingImage Frozen(Drawing drawing)
    {
        var image = new DrawingImage(drawing);
        image.Freeze();
        return image;
    }

    /// <summary>A vector drawable scaled from its viewport into <paramref name="bounds"/>, less
    /// its inset. A path WPF cannot read is left out; null when none is drawn.</summary>
    private static Drawing? VectorDrawing(ApkVector vector, Rect bounds)
    {
        var inner = new Rect(bounds.X + bounds.Width * vector.Inset, bounds.Y + bounds.Height * vector.Inset,
            bounds.Width * (1 - 2 * vector.Inset), bounds.Height * (1 - 2 * vector.Inset));
        var group = new DrawingGroup
        {
            Transform = new MatrixTransform(new Matrix(inner.Width / vector.Width, 0, 0, inner.Height / vector.Height,
                inner.X, inner.Y)),
        };
        foreach (var path in vector.Paths)
        {
            try
            {
                var geometry = PathGeometry.CreateFromGeometry(Geometry.Parse(AndroidPathData.ToWpf(path.Data)));
                geometry.FillRule = path.EvenOdd ? FillRule.EvenOdd : FillRule.Nonzero;
                var t = path.Transform;
                geometry.Transform = new MatrixTransform(new Matrix(t[0], t[1], t[2], t[3], t[4], t[5]));
                Brush? fill = path.Fill is { } f ? new SolidColorBrush(Argb(f)) : null;
                Pen? pen = path.Stroke is { } s && path.StrokeWidth > 0
                    ? new Pen(new SolidColorBrush(Argb(s)), path.StrokeWidth)
                    : null;
                group.Children.Add(new GeometryDrawing(fill, pen, geometry));
            }
            catch (Exception e) when (e is FormatException or InvalidOperationException or ArgumentException)
            {
                // Path data WPF's parser does not take.
            }
        }
        return group.Children.Count > 0 ? group : null;
    }

    /// <summary>Decodes an icon keeping its alpha. An embedded colour profile is ignored,
    /// because converting through it can flatten transparent corners to black, and the pixels
    /// are premultiplied, the format that composes the same over any tile.</summary>
    private static BitmapSource Bitmap(byte[] data)
    {
        using var stream = new MemoryStream(data);
        var decoder = BitmapDecoder.Create(stream,
            BitmapCreateOptions.IgnoreColorProfile | BitmapCreateOptions.PreservePixelFormat,
            BitmapCacheOption.OnLoad);
        var image = new FormatConvertedBitmap(decoder.Frames[0], PixelFormats.Pbgra32, null, 0);
        image.Freeze();
        return image;
    }

    /// <summary>More or Less: opens the full list, or folds it back to the first permissions
    /// with the list scrolled back to its top. The link keeps the keyboard focus.</summary>
    private void OnMore(object sender, RoutedEventArgs e)
    {
        _vm.ToggleExpanded();
        if (!_vm.IsExpanded)
        {
            DetailsScroller.ScrollToTop();
        }
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
