import Foundation

/// One alternative store, as `dsa stores` describes it.
public struct AltStoreInfo: Equatable, Sendable {
    public var id: String
    /// The store's plain name, a trademark of its owner.
    public var name: String
    public var package: String
    public var homepage: String
    public var installed: Bool
    /// The installed version; empty when it is not installed or Android does not say.
    public var version: String

    public init(id: String, name: String, package: String, homepage: String, installed: Bool, version: String) {
        self.id = id
        self.name = name
        self.package = package
        self.homepage = homepage
        self.installed = installed
        self.version = version
    }

    /// The stores, as a service from before `dsa stores` would have listed them, so the item
    /// still shows its buttons when the service cannot say where they stand.
    public static let defaults: [AltStoreInfo] = [
        AltStoreInfo(id: "onestore", name: "ONE store", package: "com.skt.skaf.A000Z00040",
                     homepage: "https://m.onestore.co.kr/", installed: false, version: ""),
        AltStoreInfo(id: "aurora", name: "Aurora Store", package: "com.aurora.store",
                     homepage: "https://auroraoss.com/", installed: false, version: ""),
        AltStoreInfo(id: "fdroid", name: "F-Droid", package: "org.fdroid.fdroid",
                     homepage: "https://f-droid.org/", installed: false, version: ""),
    ]
}

public extension DsaService {
    /// Reads what `dsa stores ... --json` printed.
    static func readStores(_ json: Json) -> [AltStoreInfo] {
        guard json["stores"].isArray else {
            return AltStoreInfo.defaults
        }
        return json["stores"].items.compactMap { s in
            let id = s.string("id")
            guard !id.isEmpty else { return nil }
            return AltStoreInfo(id: id, name: s.string("name"), package: s.string("package"),
                                homepage: s.string("homepage"), installed: s.bool("installed"),
                                version: s.string("version"))
        }
    }

    func stores() async throws -> [AltStoreInfo] {
        DsaService.readStores(try await runJson(["stores", "status"]))
    }

    /// Downloads the store's newest APK from its official site, checks its package and signer,
    /// and installs it, starting Android first when it is not running.
    func installStore(_ id: String) async throws -> [AltStoreInfo] {
        DsaService.readStores(try await runJson(["stores", "install", id]))
    }

    /// Opens the installed store in a window of its own.
    func openStore(_ package: String) async throws {
        _ = try await runText(["launch", package])
    }
}

/// The alternative stores item on the Advanced page, under Google Play: a button for each
/// store that installs it, or opens it once it is installed. A store is removed from the app
/// list like any other app.
@MainActor
public final class StoresViewModel: ObservableObject {
    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var stores: [AltStoreInfo] = AltStoreInfo.defaults
    @Published public private(set) var installing: String?
    @Published public private(set) var errors: [String: String] = [:]

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var isBusy: Bool { installing != nil }

    public func canInstall(_ store: AltStoreInfo) -> Bool { !isBusy && !store.installed }
    public func canOpen(_ store: AltStoreInfo) -> Bool { !isBusy && store.installed }

    /// One line on where the store stands.
    public func status(_ store: AltStoreInfo) -> String {
        if installing == store.id {
            return text.get("AltStore_Installing")
        }
        if let error = errors[store.id] {
            return text.format("AltStore_Failed", error)
        }
        if store.installed {
            return store.version.isEmpty ? text.get("AltStore_Installed") : text.format("AltStore_InstalledVersion", store.version)
        }
        return text.get("AltStore_NotInstalled")
    }

    /// Reads where the stores stand. A service that cannot say leaves every store offered for
    /// install.
    public func load() async {
        stores = (try? await service.stores()) ?? AltStoreInfo.defaults
    }

    public func install(_ id: String) async {
        guard !isBusy, stores.contains(where: { $0.id == id }) else { return }
        installing = id
        errors[id] = nil
        do {
            stores = try await service.installStore(id)
        } catch {
            errors[id] = Format.problem(text, error)
        }
        installing = nil
    }

    public func open(_ id: String) async {
        guard let store = stores.first(where: { $0.id == id }), store.installed else { return }
        errors[id] = nil
        do {
            try await service.openStore(store.package)
        } catch {
            errors[id] = Format.problem(text, error)
        }
    }
}
