using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Automation.Provider;
using System.Windows.Controls;

namespace Dsa.ApkInstaller;

/// <summary>
/// The More and Less link under the permissions. It is a button, so Tab reaches it and Enter
/// or Space presses it, and to UI Automation it is also an expand and collapse control whose
/// state follows <see cref="IsExpanded"/>, so a screen reader says whether the list is open.
/// </summary>
public sealed class ExpandLink : Button
{
    public static readonly DependencyProperty IsExpandedProperty = DependencyProperty.Register(
        nameof(IsExpanded), typeof(bool), typeof(ExpandLink), new PropertyMetadata(false, OnIsExpandedChanged));

    public bool IsExpanded
    {
        get => (bool)GetValue(IsExpandedProperty);
        set => SetValue(IsExpandedProperty, value);
    }

    internal static ExpandCollapseState State(bool expanded) =>
        expanded ? ExpandCollapseState.Expanded : ExpandCollapseState.Collapsed;

    protected override AutomationPeer OnCreateAutomationPeer() => new ExpandLinkPeer(this);

    private static void OnIsExpandedChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (UIElementAutomationPeer.FromElement((ExpandLink)d) is ExpandLinkPeer peer)
        {
            peer.RaisePropertyChangedEvent(ExpandCollapsePatternIdentifiers.ExpandCollapseStateProperty,
                State((bool)e.OldValue), State((bool)e.NewValue));
        }
    }
}

/// <summary>The link to UI Automation: a button that can also be expanded and collapsed.
/// Expanding or collapsing presses it, as a click does.</summary>
internal sealed class ExpandLinkPeer : ButtonAutomationPeer, IExpandCollapseProvider
{
    public ExpandLinkPeer(ExpandLink owner) : base(owner)
    {
    }

    private ExpandLink Link => (ExpandLink)Owner;

    public override object GetPattern(PatternInterface patternInterface) =>
        patternInterface == PatternInterface.ExpandCollapse ? this : base.GetPattern(patternInterface);

    public ExpandCollapseState ExpandCollapseState => ExpandLink.State(Link.IsExpanded);

    public void Expand()
    {
        if (!Link.IsExpanded)
        {
            ((IInvokeProvider)this).Invoke();
        }
    }

    public void Collapse()
    {
        if (Link.IsExpanded)
        {
            ((IInvokeProvider)this).Invoke();
        }
    }
}
