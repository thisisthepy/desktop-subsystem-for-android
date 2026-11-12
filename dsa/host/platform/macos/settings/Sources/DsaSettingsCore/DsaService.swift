import Foundation

/// Asks the DSA service, through the dsa command, for what the pages show, and changes the
/// settings through it. Every change goes through the service; the app writes no settings
/// file of its own.
public final class DsaService {
    public let cli: DsaCli

    public init(cli: DsaCli) {
        self.cli = cli
    }

    /// Runs dsa with `args` and `--json`, and reads what it printed.
    public func runJson(_ args: [String]) async throws -> Json {
        let result = try await cli.run(args + ["--json"])
        if result.exitCode != 0 {
            throw DsaError.failure(result)
        }
        return try Json.parse(result.output)
    }

    /// Runs dsa with `args` as they are, and gives what it printed.
    public func runText(_ args: [String]) async throws -> String {
        let result = try await cli.run(args)
        if result.exitCode != 0 {
            throw DsaError.failure(result)
        }
        return result.output
    }

    // MARK: State

    static func readStatus(_ json: Json) -> StatusInfo {
        StatusInfo(
            state: SessionState.parse(json.optionalString("state")),
            lastReason: json.optionalString("last_reason"),
            updateNotice: json.optionalString("update_notice"),
            problem: json.optionalString("problem"),
            programUpdate: readProgramUpdate(json["program_update"]))
    }

    static func readProgramUpdate(_ json: Json) -> ProgramUpdateProgress? {
        let phase = ProgramUpdatePhase.parse(json.optionalString("phase"))
        return phase == .none ? nil : ProgramUpdateProgress(phase: phase, version: json.string("version"))
    }

    public func status() async throws -> StatusInfo { DsaService.readStatus(try await runJson(["status"])) }

    /// The status from a running service only. While setup has stopped the service this fails
    /// rather than start the old service again under setup.
    public func runningStatus() async throws -> StatusInfo {
        DsaService.readStatus(try await runJson(["status", "--no-start"]))
    }

    /// Starts Android and waits until it has started or failed.
    public func start() async throws -> StatusInfo {
        DsaService.readStatus(try await runJson(["start", "--wait", "--timeout", "360"]))
    }

    /// Stops Android and waits until it has stopped.
    public func stop() async throws -> StatusInfo {
        DsaService.readStatus(try await runJson(["stop", "--wait", "--timeout", "120"]))
    }

    /// Freezes Android now: every process stops until one of its windows is shown again.
    public func freeze() async throws -> StatusInfo { DsaService.readStatus(try await runJson(["freeze"])) }

    // MARK: Settings

    static func readSettings(_ json: Json) -> SettingsInfo {
        var s = SettingsInfo()
        s.renderGpu = json.string("render_gpu")
        s.freeze = json.bool("freeze")
        s.programUpdates = json.bool("program_updates")
        s.imageUpdates = json.bool("image_updates")
        s.programUpdatesAvailable = json.bool("program_updates_available")
        s.prereleaseUpdates = json.bool("prerelease_updates")
        s.updateChannelAvailable = json.bool("update_channel_available")
        s.imageUpdatesAvailable = json.bool("image_updates_available")
        s.version = json.string("version")
        s.logDir = json.optionalString("log_dir")
        s.clipboard = json.bool("clipboard", missing: true)
        s.linksInAndroid = json.bool("links_in_android")
        s.androidVersion = json.optionalString("android_version")
        s.developerMode = json.bool("developer_mode")
        let port = json.int("adb_port")
        s.adbPort = port > 0 ? port : SettingsInfo.defaultAdbPort
        s.inputTrace = json.bool("input_trace")
        s.resizeTrace = json.bool("resize_trace")
        s.memoryLimitMb = json.int("memory_limit_mb")
        s.armTranslation = json.bool("arm_translation", missing: true)
        s.stopWithoutAsking = json.bool("stop_without_asking")
        s.advancedNetworking = json.bool("advanced_networking")
        s.network = json["network"].isObject ? readNetwork(json["network"]) : nil
        s.fileSharing = json.bool("file_sharing", missing: true)
        s.sharedFolders = json["shared_folders"].isArray ? json.strings("shared_folders") : nil
        s.adbAutoConnect = json.bool("adb_auto_connect", missing: true)
        s.adbAutoConnectState = AdbAutoConnectState.read(json.optionalString("adb_auto_connect_state"))
        s.rootElevation = json.bool("root_elevation")
        s.imageAutoRestart = json.bool("image_auto_restart", missing: true)
        s.hostCertificates = json.bool("host_certificates", missing: true)
        s.importedCertificates = json["imported_certificates"].items.filter(\.isObject).map(readImportedCertificate)
        return s
    }

    /// One root certificate as the settings list the ones the service found.
    static func readImportedCertificate(_ json: Json) -> ImportedCertificate {
        ImportedCertificate(
            subject: json.string("subject"), issuer: json.string("issuer"), source: json.string("source"),
            location: json.string("location"), sha256: json.string("sha256"), notAfter: json.int64("not_after"))
    }

    /// The network as `dsa network --json` and the settings print it.
    public static func readNetwork(_ json: Json) -> NetworkInfo {
        let mode: NetworkMode
        switch json.optionalString("mode") {
        case "nat": mode = .nat
        case "advanced": mode = .advanced
        case "off": mode = .off
        default: mode = .unknown
        }
        let problem = json.string("problem")
        let dnsResolver: DnsResolver
        switch json.optionalString("dns_resolver") {
        case "host": dnsResolver = .host
        case "network": dnsResolver = .network
        default: dnsResolver = .unknown
        }
        return NetworkInfo(
            mode: mode, addresses: json.strings("addresses"), dnsServers: json.strings("dns_servers"),
            gateway: json.string("gateway"), problem: problem.isEmpty ? nil : problem, dnsResolver: dnsResolver)
    }

    public func settings() async throws -> SettingsInfo { DsaService.readSettings(try await runJson(["settings"])) }

    private func set(_ name: String, _ value: String) async throws -> SettingsInfo {
        DsaService.readSettings(try await runJson(["settings", "set", name, value]))
    }

    private func onOff(_ on: Bool) -> String { on ? "on" : "off" }

    public func network() async throws -> NetworkInfo { DsaService.readNetwork(try await runJson(["network"])) }
    public func setAdvancedNetworking(_ on: Bool) async throws -> SettingsInfo { try await set("advanced-networking", onOff(on)) }
    /// Whether Android trusts the root certificates a user or an administrator added to this
    /// Mac. It applies at once, without restarting Android.
    public func setHostCertificates(_ on: Bool) async throws -> SettingsInfo { try await set("host-certificates", onOff(on)) }
    public func setRenderGpu(_ value: String) async throws -> SettingsInfo { try await set("render-gpu", value) }
    public func setFreeze(_ on: Bool) async throws -> SettingsInfo { try await set("freeze", onOff(on)) }
    public func setLinksInAndroid(_ on: Bool) async throws -> SettingsInfo { try await set("links-in-android", onOff(on)) }
    public func setClipboard(_ on: Bool) async throws -> SettingsInfo { try await set("clipboard", onOff(on)) }
    public func setDeveloperMode(_ on: Bool) async throws -> SettingsInfo { try await set("developer-mode", onOff(on)) }
    /// Whether stopping an app from the menu bar skips the question that confirms it.
    public func setStopWithoutAsking(_ on: Bool) async throws -> SettingsInfo { try await set("stop-without-asking", onOff(on)) }
    public func setAdbPort(_ port: Int) async throws -> SettingsInfo { try await set("adb-port", String(port)) }
    /// Whether developer mode's port is attached to this Mac's adb server by itself.
    public func setAdbAutoConnect(_ on: Bool) async throws -> SettingsInfo { try await set("adb-auto-connect", onOff(on)) }
    public func setArmTranslation(_ on: Bool) async throws -> SettingsInfo { try await set("arm-translation", onOff(on)) }
    /// Whether the chosen Mac folders show in Android's shared storage.
    public func setFileSharing(_ on: Bool) async throws -> SettingsInfo { try await set("file-sharing", onOff(on)) }
    /// Which folders are shared, in the order the service keeps them.
    public func setSharedFolders(_ folders: [String]) async throws -> SettingsInfo {
        let list = folders.joined(separator: ",")
        return try await set("shared-folders", list.isEmpty ? "none" : list)
    }
    public func setProgramUpdates(_ on: Bool) async throws -> SettingsInfo { try await set("program-updates", onOff(on)) }
    public func setImageUpdates(_ on: Bool) async throws -> SettingsInfo { try await set("image-updates", onOff(on)) }
    /// Whether Android restarts by itself into an Android image the service downloaded.
    public func setImageAutoRestart(_ on: Bool) async throws -> SettingsInfo { try await set("image-auto-restart", onOff(on)) }
    /// Chooses the release channel: on takes pre-releases as well as stable releases.
    public func setPrereleaseUpdates(_ on: Bool) async throws -> SettingsInfo { try await set("prerelease-updates", onOff(on)) }
    public func setInputTrace(_ on: Bool) async throws -> SettingsInfo { try await set("input-trace", onOff(on)) }
    public func setResizeTrace(_ on: Bool) async throws -> SettingsInfo { try await set("resize-trace", onOff(on)) }
    /// Lets apps in Android ask for root through an administrator's approval, or not. Only an
    /// administrator changes it; it applies from the next start of Android.
    public func setRootElevation(_ on: Bool) async throws -> SettingsInfo { try await set("root-elevation", onOff(on)) }
    /// Sets the most memory the user's Android may grow to, in MiB; 0 is automatic.
    public func setMemoryLimit(_ mb: Int) async throws -> SettingsInfo { try await set("memory-limit", mb == 0 ? "auto" : String(mb)) }

    /// Erases the user's Android data: the service stops Android and puts a fresh data disk in
    /// place of theirs. The Android image and the settings stay.
    public func resetData() async throws { _ = try await runJson(["reset-data", "--yes"]) }

    /// Opens Android's shared storage in the Finder, starting Android when it is not running,
    /// and returns the path the Finder opened.
    public func openAndroidFiles() async throws -> String { try await runJson(["files", "open"]).string("path") }

    // MARK: ARM translation

    private static func readArmPackage(_ p: Json) -> ArmPackageInfo? {
        p.isObject
            ? ArmPackageInfo(name: p.string("name"), version: p.string("version"), library: p.string("library"), license: p.string("license"))
            : nil
    }

    static func readArmTranslation(_ json: Json) -> ArmTranslationInfo {
        let package = readArmPackage(json["package"])
        let enabled = json.bool("enabled", missing: true)
        let a = json["android"]
        var translator: ArmTranslatorInfo?
        if json.has("translator") {
            let t = json["translator"]
            if t.isObject, let source = ArmSource(rawValue: t.string("source")) {
                translator = ArmTranslatorInfo(source: source, name: t.string("name"), version: t.string("version"),
                                               license: t.string("license"))
            }
        } else if let package, enabled {
            // A dsa from before the default translator: the installed package, while on.
            translator = ArmTranslatorInfo(source: .user, name: package.name, version: package.version, license: package.license)
        }
        return ArmTranslationInfo(
            package: package,
            enabled: enabled,
            supported: json.bool("supported"),
            android: a.isObject ? ArmGuestInfo(installed: a.bool("installed"), active: a.bool("active"), abilist: a.string("abilist")) : nil,
            restartNeeded: json.bool("restart_needed"),
            googleFetchAvailable: json.bool("google_fetch_available"),
            defaultPackage: readArmPackage(json["default"]),
            translator: translator)
    }

    public func armTranslation() async throws -> ArmTranslationInfo {
        DsaService.readArmTranslation(try await runJson(["arm-translation", "status"]))
    }

    /// Removes the ARM translation package; Android goes without it from its next start, back
    /// to the default translator where the image has one.
    public func removeArmTranslation() async throws -> ArmTranslationInfo {
        DsaService.readArmTranslation(try await runJson(["arm-translation", "remove"]))
    }

    /// Goes back to the default translator the image carries, removing the package installed
    /// in its place. It applies from the next start of Android.
    public func useDefaultArmTranslator() async throws -> ArmTranslationInfo {
        DsaService.readArmTranslation(try await runJson(["arm-translation", "use-default"]))
    }

    /// Google's terms for what its SDK repository gives, the Android SDK License, which cover
    /// its ARM translator.
    public static let googleArmTermsUrl = "https://developer.android.com/studio/terms"

    /// Where the emulator image Google's ARM translator comes from is downloaded.
    public static let googleArmSource = "https://dl.google.com/android/repository/sys-img/google_apis/"

    /// Downloads Google's emulator image for this Android from Google, takes its ARM translator
    /// out and installs it, starting Android first when it is not running. The user agreed to
    /// Google's terms in the app first, so dsa does not ask again. It takes minutes.
    public func fetchGoogleArmTranslator() async throws -> ArmTranslationInfo {
        DsaService.readArmTranslation(try await runJson(["arm-translation", "fetch", "google", "--yes"]))
    }

    // MARK: Memory and GPUs

    static func readMemory(_ json: Json) -> MemoryReport {
        let host = json["host"].items.compactMap { p -> HostProcess? in
            guard let role = HostRole.parse(p.optionalString("role")) else {
                return nil
            }
            return HostProcess(role: role, pid: UInt32(truncatingIfNeeded: p.uint("pid")),
                               workingSetKb: p.uint("working_set_kb"), privateKb: p.uint("private_kb"))
        }
        var guest: GuestMemoryInfo?
        let g = json["guest"]
        if g.isObject {
            // What processes cost: resident_kb, and compressed_kb, null when zram's compression
            // is unknown.
            func share(_ s: Json) -> MemoryShare {
                MemoryShare(residentKb: s.uint("resident_kb"), compressedKb: s.optionalUInt("compressed_kb"))
            }
            let top = g["top_processes"].items.map {
                GuestProcess(pid: $0.int("pid"), name: $0.string("name"), pssKb: $0.uint("pss_kb"), swapPssKb: $0.uint("swap_pss_kb"),
                             googlePlay: $0.bool("google_play"), cost: $0.has("resident_kb") ? share($0) : nil)
            }
            let k = g["kernel"]
            let kernel = k.isObject
                ? GuestKernelInfo(
                    slabUnreclaimableKb: k.uint("slab_unreclaimable_kb"), kernelStackKb: k.uint("kernel_stack_kb"),
                    pageTablesKb: k.uint("page_tables_kb"), vmallocUsedKb: k.uint("vmalloc_used_kb"),
                    percpuKb: k.uint("percpu_kb"), cmaTotalKb: k.uint("cma_total_kb"),
                    memOnlineKb: k.uint("mem_online_kb"), unaccountedKb: k.uint("unaccounted_kb"),
                    reservedAtBootKb: k.optionalUInt("reserved_at_boot_kb"))
                : nil
            guest = GuestMemoryInfo(
                totalKb: g.uint("mem_total_kb"), freeKb: g.uint("mem_free_kb"), availableKb: g.uint("mem_available_kb"),
                buffersKb: g.uint("buffers_kb"), cachedKb: g.uint("cached_kb"), anonPagesKb: g.uint("anon_pages_kb"),
                shmemKb: g.uint("shmem_kb"), slabReclaimableKb: g.uint("slab_reclaimable_kb"),
                swapTotalKb: g.uint("swap_total_kb"), swapFreeKb: g.uint("swap_free_kb"),
                zramOriginalKb: g.uint("zram_original_kb"), zramUsedKb: g.uint("zram_used_kb"),
                totalPssKb: g.uint("total_pss_kb"), topProcesses: top, ageSeconds: g.uint("age_seconds"), kernel: kernel,
                apps: g["apps"].isObject ? share(g["apps"]) : nil,
                googlePlay: g["google_play"].isObject
                    ? GooglePlayMemory(processes: g["google_play"].uint("processes"), cost: share(g["google_play"]))
                    : nil)
        }
        return MemoryReport(guest: guest, host: host, hostTotalKb: json.uint("host_total_kb"),
                            vmBeyondGuestKb: json.optionalUInt("vm_beyond_guest_kb"))
    }

    public func memory() async throws -> MemoryReport { DsaService.readMemory(try await runJson(["memory"])) }

    public func gpus() async throws -> [GpuInfo] {
        try await runJson(["gpus"])["gpus"].items.map {
            GpuInfo(name: $0.string("name"), luid: $0.string("luid"), primaryDisplay: $0.bool("primary_display"))
        }
    }

    // MARK: Installing

    /// Installs the APK at `path` in Android, starting Android first when it is not running.
    /// A refusal by Android is a report with its reason; anything else that keeps the APK from
    /// Android is a `DsaError`.
    public func install(_ path: String) async throws -> InstallReport {
        let result = try await cli.run(["install", path, "--json"])
        if result.exitCode != 0 {
            let detail = DsaError.detail(of: result)
            if let refused = DsaService.refusal(detail, verb: "installed") {
                return InstallReport(installed: false, status: refused.reason, package: nil, detail: refused.detail)
            }
            throw DsaError.failure(result)
        }
        let json = try Json.parse(result.output)
        return InstallReport(installed: json.bool("installed"), status: json.string("status"),
                             package: json.optionalString("package"), detail: json.string("message"))
    }

    /// Reads dsa's line for something Android refused, `not <verb> (<reason>): <detail>`.
    static func refusal(_ line: String, verb: String) -> (reason: String, detail: String)? {
        let start = "not \(verb) ("
        guard line.hasPrefix(start), let close = line.range(of: "): ") else {
            return nil
        }
        let reason = String(line[line.index(line.startIndex, offsetBy: start.count)..<close.lowerBound])
        guard !reason.isEmpty, reason.allSatisfy({ ("a"..."z").contains($0) || $0 == "-" }) else {
            return nil
        }
        return (reason, String(line[close.upperBound...]))
    }

    // MARK: Widgets

    static func readPlaced(_ json: Json) -> PlacedWidget {
        let p = json["placement"]
        return PlacedWidget(id: json.int("id"), provider: json.string("provider"), label: json.string("label"),
                            appLabel: json.string("app"), x: p.int("x"), y: p.int("y"),
                            widthDp: p.int("width_dp"), heightDp: p.int("height_dp"))
    }

    /// Reads one line of `dsa widgets watch --json`; nil for a line that is no change it knows.
    public static func parseWidgetChange(_ line: String) -> WidgetChange? {
        guard let json = try? Json.parse(line), json.isObject else {
            return nil
        }
        switch json.string("event") {
        case "widgets": return .all(json["widgets"].items.map(readPlaced))
        case "placed" where json["widget"].isObject: return .placed(readPlaced(json["widget"]))
        case "removed": return .removed(json.int("id"))
        default: return nil
        }
    }

    /// The widgets the apps in Android offer and the ones placed, starting Android first when
    /// it is not running.
    public func widgets() async throws -> WidgetList {
        let json = try await runJson(["widgets", "list"])
        let offers = json["providers"].items.map {
            WidgetOffer(provider: $0.string("provider"), package: $0.string("package"), label: $0.string("label"),
                        previewPng: $0.base64("preview_png"), defaultWidthDp: $0.int("default_width_dp"),
                        defaultHeightDp: $0.int("default_height_dp"), needsConfiguration: $0.bool("needs_configuration"),
                        appLabel: $0.string("app"))
        }
        return WidgetList(offers: offers, placed: json["widgets"].items.map(DsaService.readPlaced))
    }

    /// Places a widget of `provider` on the desktop. When its app asks for a setup, this waits
    /// for the user to finish it.
    public func addWidget(_ provider: String) async throws -> PlacedWidget {
        DsaService.readPlaced(try await runJson(["widgets", "add", provider]))
    }

    public func removeWidget(_ id: Int) async throws { _ = try await runJson(["widgets", "remove", String(id)]) }

    // MARK: Background

    /// The apps that run in the background in Android, and its media sessions. It never
    /// starts Android; one that is not running has none.
    public func background() async throws -> BackgroundList {
        let json = try await runJson(["background", "list"])
        let apps = json["apps"].items.map {
            BackgroundApp(package: $0.string("package"), label: $0.string("label"), title: $0.string("title"),
                          text: $0.string("text"), actions: $0.strings("actions"), iconPng: $0.base64("icon_png"))
        }
        let media = json["media"].items.map {
            MediaInfo(package: $0.string("package"), label: $0.string("label"), title: $0.string("title"),
                      artist: $0.string("artist"), album: $0.string("album"), playback: $0.string("playback"))
        }
        return BackgroundList(apps: apps, media: media)
    }

    public func stopBackground(_ package: String) async throws { _ = try await runJson(["background", "stop", package]) }
    public func openBackground(_ package: String) async throws { _ = try await runJson(["background", "open", package]) }

    // MARK: Logs and updates

    /// Saves the diagnostics of Android and of DSA on this Mac to `path`, as one zip archive.
    public func collectDiagnostics(_ path: String) async throws -> String {
        try await runText(["logs", "collect", "-o", path])
    }

    /// Installs the newest version of DSA now. Returns the version setup installs, or none
    /// when DSA is up to date, with what the service said.
    public func updateNow() async throws -> ProgramUpdateAnswer {
        let json = try await runJson(["update"])
        let installing = json.optionalString("installing") ?? ""
        return ProgramUpdateAnswer(installing: installing.isEmpty ? nil : installing, detail: json.string("detail"))
    }
}
