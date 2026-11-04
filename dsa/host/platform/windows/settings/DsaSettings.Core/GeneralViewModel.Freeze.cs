namespace Dsa.Settings.Core;

/// <summary>Freezing Android now, as <c>dsa freeze</c> does.</summary>
public sealed partial class GeneralViewModel
{
    /// <summary>Whether Android runs and can be frozen now.</summary>
    public bool CanFreeze => !_busy && State is SessionState.Active or SessionState.Dozing;

    /// <summary>Stops every process of Android until one of its windows is shown again.</summary>
    public Task FreezeAsync() => RunAsync(_service.FreezeAsync);
}
