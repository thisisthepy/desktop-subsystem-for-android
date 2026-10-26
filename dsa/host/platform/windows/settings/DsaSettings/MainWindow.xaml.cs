using Dsa.Settings.Pages;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.Graphics;

namespace Dsa.Settings;

public sealed partial class MainWindow : Window
{
    private static readonly Dictionary<string, Type> Pages = new()
    {
        ["General"] = typeof(GeneralPage),
        ["Memory"] = typeof(MemoryPage),
        ["Display"] = typeof(DisplayPage),
        ["Updates"] = typeof(UpdatesPage),
        ["Advanced"] = typeof(AdvancedPage),
        ["About"] = typeof(AboutPage),
    };

    public MainWindow()
    {
        InitializeComponent();
        Title = App.Text.Get("WindowTitle");
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        AppWindow.Resize(new SizeInt32(1040, 760));
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "logo.ico");
        if (File.Exists(icon))
        {
            AppWindow.SetIcon(icon);
        }
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = 640;
            presenter.PreferredMinimumHeight = 480;
        }
        ContentFrame.Navigate(typeof(GeneralPage));
        _ = App.Settings.LoadAsync();
    }

    private void OnSelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem { Tag: string tag } && Pages.TryGetValue(tag, out var page)
            && ContentFrame.CurrentSourcePageType != page)
        {
            ContentFrame.Navigate(page, null, args.RecommendedNavigationTransitionInfo ?? new EntranceNavigationTransitionInfo());
        }
    }
}
