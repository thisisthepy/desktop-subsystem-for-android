using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>The VM's size on the host, what Android uses of it, and what the guest kernel
/// holds for itself.</summary>
public sealed partial class MemoryViewModel
{
    /// <summary>What the VM holds on the host now for itself (its working set without the pages
    /// of the disk files it maps directly, which Windows drops without writing), or empty
    /// without a VM.</summary>
    public string VmSize
    {
        get
        {
            var vm = (_report?.Host ?? Array.Empty<HostProcess>()).Where(p => p.Role == HostRole.Vm).ToList();
            return vm.Count == 0 ? "" : Format.Size((ulong)vm.Sum(p => (long)p.OwnKb));
        }
    }

    public bool HasVmSize => VmSize.Length > 0;

    /// <summary>What Android uses of its memory: all of it but what is free for the taking.</summary>
    public string GuestUsed => _report?.Guest is { } guest
        ? Format.Size(guest.TotalKb - Math.Min(guest.AvailableKb, guest.TotalKb))
        : "";

    /// <summary>Free guest memory the VM still holds on the host, which the host has not taken
    /// back yet.</summary>
    public string VmBeyondGuest => _report?.VmBeyondGuestKb is { } kb ? Format.Size(kb) : "";

    public bool HasVmBeyondGuest => _report?.VmBeyondGuestKb is not null;

    public bool KernelReported => _report?.Guest?.Kernel is not null;

    /// <summary>The kernel's own memory, part by part, as <c>dsa memory</c> shows it.</summary>
    public IReadOnlyList<MemoryRow> KernelRows
    {
        get
        {
            if (_report?.Guest?.Kernel is not { } k)
            {
                return Array.Empty<MemoryRow>();
            }
            var rows = new List<MemoryRow>
            {
                new(_text.Get("Memory_KernelSlab"), _text.Get("Memory_KernelSlabDetail"), Format.Size(k.SlabUnreclaimableKb)),
                new(_text.Get("Memory_KernelStacks"), "", Format.Size(k.KernelStackKb)),
                new(_text.Get("Memory_KernelPageTables"), "", Format.Size(k.PageTablesKb)),
                new(_text.Get("Memory_KernelVmalloc"), "", Format.Size(k.VmallocUsedKb)),
                new(_text.Get("Memory_KernelPercpu"), "", Format.Size(k.PercpuKb)),
                new(_text.Get("Memory_KernelUnaccounted"), _text.Get("Memory_KernelUnaccountedDetail"), Format.Size(k.UnaccountedKb)),
            };
            if (k.ReservedAtBootKb is { } reserved)
            {
                rows.Add(new MemoryRow(_text.Get("Memory_KernelReserved"),
                    string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_KernelReservedDetail"),
                        Format.Size(k.MemOnlineKb), Format.Size(k.CmaTotalKb)),
                    Format.Size(reserved)));
            }
            return rows;
        }
    }
}
