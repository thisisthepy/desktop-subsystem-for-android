using System.Collections.Specialized;
using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class LogsPage : Page
{
    private static LogViewModel ViewModel => App.Logs;

    public LogsPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        foreach (var priority in LogViewModel.Priorities)
        {
            PriorityBox.Items.Add(new ComboBoxItem { Content = App.Text.Get($"Logs_Priority_{priority}"), Tag = priority });
        }
        PriorityBox.SelectedIndex = Math.Max(0, LogViewModel.Priorities.ToList().IndexOf(ViewModel.Priority));
        // Android reads its log for this page only while the page is shown.
        Loaded += async (_, _) =>
        {
            ViewModel.Lines.CollectionChanged += OnLinesChanged;
            await ViewModel.StartAsync();
        };
        Unloaded += (_, _) =>
        {
            ViewModel.Lines.CollectionChanged -= OnLinesChanged;
            ViewModel.Stop();
        };
    }

    /// <summary>Keeps the newest line in view while the log is not paused.</summary>
    private void OnLinesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (ViewModel.Paused || e.Action != NotifyCollectionChangedAction.Add || LinesList.Items.Count == 0)
        {
            return;
        }
        LinesList.ScrollIntoView(LinesList.Items[^1]);
    }

    private void OnPriorityChanged(object sender, SelectionChangedEventArgs e)
    {
        if (PriorityBox.SelectedItem is ComboBoxItem { Tag: string priority })
        {
            ViewModel.Priority = priority;
        }
    }

    private async void OnSaveDiagnostics(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.SaveFileDialog
        {
            Title = App.Text.Get("Logs_SaveDialogTitle"),
            FileName = $"dsa-diagnostics-{DateTime.Now:yyyyMMdd-HHmmss}.zip",
            DefaultExt = ".zip",
            Filter = App.Text.Get("Logs_SaveDialogFilter") + "|*.zip",
        };
        if (dialog.ShowDialog(Window.GetWindow(this)) == true)
        {
            await ViewModel.SaveDiagnosticsAsync(dialog.FileName);
        }
    }
}
