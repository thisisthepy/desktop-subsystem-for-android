import Foundation

/// One line of the memory page: what it is, a detail, and its size.
public struct MemoryRow: Equatable, Hashable, Sendable {
    public var label: String
    public var detail: String
    public var size: String
}

/// The Memory page: the host processes that run Android, the VM's size, and Android's own
/// breakdown down to its kernel.
@MainActor
public final class MemoryViewModel: ObservableObject {
    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var report: MemoryReport?
    @Published public private(set) var error: String?

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var hostTotal: String { Format.size(report?.hostTotalKb ?? 0) }
    public var hostRunning: Bool { !(report?.host.isEmpty ?? true) }
    /// Whether the report says no host process of the user's Android runs.
    public var hostNotRunning: Bool { report != nil && !hostRunning }
    public var guestReported: Bool { report?.guest != nil }
    public var guestTotal: String { report?.guest.map { Format.size($0.totalKb) } ?? "" }
    public var guestAvailable: String { report?.guest.map { Format.size($0.availableKb) } ?? "" }

    private func used(_ guest: GuestMemoryInfo) -> UInt64 {
        guest.totalKb - min(guest.availableKb, guest.totalKb)
    }

    /// How full the guest's memory is, from 0 to 1.
    public var guestUsedFraction: Double {
        guard let guest = report?.guest, guest.totalKb > 0 else {
            return 0
        }
        return Double(used(guest)) / Double(guest.totalKb)
    }

    public var guestAge: String {
        report?.guest.map { text.format("Memory_ReportAge", $0.ageSeconds) } ?? ""
    }

    /// What the VM holds on the host now, or empty without a VM.
    public var vmSize: String {
        let vm = (report?.host ?? []).filter { $0.role == .vm }
        return vm.isEmpty ? "" : Format.size(vm.reduce(0) { $0 + $1.workingSetKb })
    }

    public var hasVmSize: Bool { !vmSize.isEmpty }

    /// What Android uses of its memory: all of it but what is free for the taking.
    public var guestUsed: String { report?.guest.map { Format.size(used($0)) } ?? "" }

    /// Free guest memory the VM still holds on the host, which the host has not taken back yet.
    public var vmBeyondGuest: String { report?.vmBeyondGuestKb.map { Format.size($0) } ?? "" }
    public var hasVmBeyondGuest: Bool { report?.vmBeyondGuestKb != nil }
    public var kernelReported: Bool { report?.guest?.kernel != nil }

    public var hostRows: [MemoryRow] {
        (report?.host ?? []).map {
            MemoryRow(label: text.get("Role_\($0.role.rawValue)"),
                      detail: text.format("Memory_ProcessDetail", $0.pid, Format.size($0.privateKb)),
                      size: Format.size($0.workingSetKb))
        }
    }

    /// Android's breakdown: the apps and the system, the caches, the kernel's reclaimable
    /// caches, shared memory and compressed swap, and what is free.
    public var guestRows: [MemoryRow] {
        guard let g = report?.guest else {
            return []
        }
        return [
            MemoryRow(label: text.get("Memory_Apps"), detail: text.get("Memory_AppsDetail"), size: Format.size(g.totalPssKb)),
            MemoryRow(label: text.get("Memory_Cache"), detail: text.get("Memory_CacheDetail"), size: Format.size(g.cachedKb + g.buffersKb)),
            MemoryRow(label: text.get("Memory_Kernel"), detail: text.get("Memory_KernelDetail"), size: Format.size(g.slabReclaimableKb)),
            MemoryRow(label: text.get("Memory_Shared"), detail: "", size: Format.size(g.shmemKb)),
            MemoryRow(label: text.get("Memory_Zram"), detail: text.format("Memory_ZramDetail", Format.size(g.zramOriginalKb)),
                      size: Format.size(g.zramUsedKb)),
            MemoryRow(label: text.get("Memory_Free"), detail: "", size: Format.size(g.freeKb)),
        ]
    }

    /// The kernel's own memory, part by part, as `dsa memory` shows it.
    public var kernelRows: [MemoryRow] {
        guard let k = report?.guest?.kernel else {
            return []
        }
        var rows = [
            MemoryRow(label: text.get("Memory_KernelSlab"), detail: text.get("Memory_KernelSlabDetail"), size: Format.size(k.slabUnreclaimableKb)),
            MemoryRow(label: text.get("Memory_KernelStacks"), detail: "", size: Format.size(k.kernelStackKb)),
            MemoryRow(label: text.get("Memory_KernelPageTables"), detail: "", size: Format.size(k.pageTablesKb)),
            MemoryRow(label: text.get("Memory_KernelVmalloc"), detail: "", size: Format.size(k.vmallocUsedKb)),
            MemoryRow(label: text.get("Memory_KernelPercpu"), detail: "", size: Format.size(k.percpuKb)),
            MemoryRow(label: text.get("Memory_KernelUnaccounted"), detail: text.get("Memory_KernelUnaccountedDetail"), size: Format.size(k.unaccountedKb)),
        ]
        if let reserved = k.reservedAtBootKb {
            rows.append(MemoryRow(
                label: text.get("Memory_KernelReserved"),
                detail: text.format("Memory_KernelReservedDetail", Format.size(k.memOnlineKb), Format.size(k.cmaTotalKb)),
                size: Format.size(reserved)))
        }
        return rows
    }

    public var guestProcessRows: [MemoryRow] {
        (report?.guest?.topProcesses ?? []).map {
            MemoryRow(label: $0.name, detail: text.format("Memory_GuestProcessDetail", $0.pid), size: Format.size($0.pssKb))
        }
    }

    public func refresh() async {
        do {
            report = try await service.memory()
            error = nil
        } catch {
            self.error = Format.problem(text, error)
        }
    }
}
