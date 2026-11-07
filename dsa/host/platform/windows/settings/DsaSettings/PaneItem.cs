using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Automation.Provider;
using System.Windows.Media;
using Wpf.Ui.Controls;

namespace Dsa.Settings;

/// <summary>An item of the navigation pane that UI Automation clients can find and use.
/// The library's item gives automation no name and no way to select it, so a screen reader
/// announced an unnamed element and could not open a page with it. This item is named by its
/// label, and selecting or invoking it opens its page, as a click does. It looks the same:
/// the library's style and the pane's template still apply.</summary>
public class PaneItem : NavigationViewItem
{
    protected override AutomationPeer OnCreateAutomationPeer() => new PaneItemAutomationPeer(this);

    protected override void OnPropertyChanged(DependencyPropertyChangedEventArgs e)
    {
        base.OnPropertyChanged(e);
        if (e.Property == IsActiveProperty
            && UIElementAutomationPeer.FromElement(this) is PaneItemAutomationPeer peer)
        {
            peer.RaisePropertyChangedEvent(SelectionItemPatternIdentifiers.IsSelectedProperty,
                e.OldValue is true, e.NewValue is true);
            peer.RaiseAutomationEvent(e.NewValue is true
                ? AutomationEvents.SelectionItemPatternOnElementSelected
                : AutomationEvents.SelectionItemPatternOnElementRemovedFromSelection);
        }
    }

    /// <summary>Opens this item's page in the navigation view that holds it.</summary>
    internal void Open()
    {
        if (!IsEnabled)
        {
            throw new ElementNotEnabledException();
        }
        if (TargetPageType is not { } page)
        {
            return;
        }
        DependencyObject? node = this;
        while (node is not null and not NavigationView)
        {
            node = VisualTreeHelper.GetParent(node) ?? LogicalTreeHelper.GetParent(node);
        }
        (node as NavigationView)?.Navigate(page);
    }
}

/// <summary>A list item named by the pane item's label, selected while its page shows.</summary>
public sealed class PaneItemAutomationPeer : FrameworkElementAutomationPeer, ISelectionItemProvider, IInvokeProvider
{
    public PaneItemAutomationPeer(PaneItem owner) : base(owner)
    {
    }

    private PaneItem Item => (PaneItem)Owner;

    protected override string GetClassNameCore() => nameof(PaneItem);

    protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.ListItem;

    protected override string GetNameCore()
    {
        var name = base.GetNameCore();
        return string.IsNullOrEmpty(name) ? Item.Content?.ToString() ?? "" : name;
    }

    protected override bool IsKeyboardFocusableCore() => Item.Focusable;

    public override object? GetPattern(PatternInterface patternInterface) => patternInterface switch
    {
        PatternInterface.SelectionItem or PatternInterface.Invoke => this,
        _ => base.GetPattern(patternInterface),
    };

    public bool IsSelected => Item.IsActive;

    public IRawElementProviderSimple? SelectionContainer =>
        ItemsControl.ItemsControlFromItemContainer(Item) is PaneItemsControl list
        && UIElementAutomationPeer.CreatePeerForElement(list) is { } peer
            ? ProviderFromPeer(peer)
            : null;

    public void Select() => Item.Open();

    public void AddToSelection()
    {
        // One page shows at a time, so adding to the selection is selecting.
        if (!Item.IsActive)
        {
            Item.Open();
        }
    }

    public void RemoveFromSelection()
    {
        // A page stays open until another opens: there is no empty selection to go back to.
        if (Item.IsActive)
        {
            throw new InvalidOperationException("one page is always open; select another item instead");
        }
    }

    public void Invoke() => Item.Open();
}

/// <summary>The list that holds the navigation pane's items.
/// A plain ItemsControl gives UI Automation a generic data item for each entry, a wrapper with
/// neither the item's name nor a way to select it, so the pane item's own automation peer never
/// reached a screen reader. This list exposes the items' own peers as its children instead.</summary>
public class PaneItemsControl : ItemsControl
{
    protected override AutomationPeer OnCreateAutomationPeer() => new PaneItemsControlAutomationPeer(this);
}

/// <summary>A list whose children are the pane items themselves, with the open page's item
/// as its selection.</summary>
public sealed class PaneItemsControlAutomationPeer : FrameworkElementAutomationPeer, ISelectionProvider
{
    public PaneItemsControlAutomationPeer(PaneItemsControl owner) : base(owner)
    {
    }

    private PaneItemsControl List => (PaneItemsControl)Owner;

    protected override string GetClassNameCore() => nameof(PaneItemsControl);

    // The children stay the element peer's: it walks the visual tree, and the items are their
    // own containers, so it reaches each pane item's own peer, named by its label, selectable
    // and invokable.
    protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.List;

    public override object? GetPattern(PatternInterface patternInterface) =>
        patternInterface == PatternInterface.Selection ? this : base.GetPattern(patternInterface);

    public bool CanSelectMultiple => false;

    // The pane has two lists (the pages and the footer), and only one of them holds the open page.
    public bool IsSelectionRequired => false;

    public IRawElementProviderSimple[] GetSelection() => List.Items.OfType<PaneItem>()
        .Where(item => item.IsActive)
        .Select(item => UIElementAutomationPeer.CreatePeerForElement(item))
        .OfType<AutomationPeer>()
        .Select(ProviderFromPeer)
        .ToArray();
}
