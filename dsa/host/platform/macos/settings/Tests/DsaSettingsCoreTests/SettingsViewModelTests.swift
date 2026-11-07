import XCTest
@testable import DsaSettingsCore

/// The General, Display, Updates, Network, Advanced and Memory pages, through the dsa command
/// the Windows settings app runs as well.
@MainActor
final class SettingsViewModelTests: XCTestCase {
    private func make() -> (FakeCli, DsaService) {
        let cli = FakeCli()
        return (cli, DsaService(cli: cli))
    }

    func test_fr17_general_page_shows_the_state_the_service_reports() async {
        let (cli, service) = make()
        cli.answer("status --json", #"{"state":"Active","last_reason":"stopped by the user"}"#)
        let general = GeneralViewModel(service: service, text: KeyText())
        await general.refresh()
        XCTAssertEqual(general.state, .active)
        XCTAssertEqual(general.stateText, "State_Active")
        XCTAssertTrue(general.canStop)
        XCTAssertFalse(general.canStart)
        XCTAssertTrue(general.canFreeze)
        XCTAssertEqual(general.lastReason, "stopped by the user")
        XCTAssertNil(general.error)
    }

    func test_fr17_start_and_stop_wait_for_the_service() async {
        let (cli, service) = make()
        cli.answer("start --wait --timeout 360 --json", #"{"state":"Active"}"#)
        cli.answer("stop --wait --timeout 120 --json", #"{"state":"Stopped"}"#)
        let general = GeneralViewModel(service: service, text: KeyText())
        await general.start()
        XCTAssertEqual(general.state, .active)
        await general.stop()
        XCTAssertEqual(general.state, .stopped)
        XCTAssertTrue(general.canStart)
    }

    func test_fr17_a_service_that_is_not_running_is_said_in_the_users_language() async {
        let (cli, service) = make()
        cli.fail("status --json", "dsa: cannot reach the DSA service: no socket")
        let general = GeneralViewModel(service: service, text: KeyText())
        await general.refresh()
        XCTAssertEqual(general.error, "Problem_ServiceUnreachable (cannot reach the DSA service: no socket)")
    }

    func test_fr17_choosing_a_render_gpu_goes_through_the_local_api() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"render_gpu":"display","version":"0.1.0"}"#)
        cli.answer("arm-translation status --json", #"{"supported":false}"#)
        cli.answer("gpus --json", #"{"gpus":[{"name":"Apple M2","luid":"1","primary_display":true}]}"#)
        cli.answer("settings set render-gpu discrete --json", #"{"render_gpu":"discrete"}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.gpuChoices.map(\.value), ["display", "integrated", "discrete", "Apple M2"])
        XCTAssertEqual(settings.displayGpuName, "Apple M2")
        await settings.setRenderGpu("discrete")
        XCTAssertTrue(cli.called("settings set render-gpu discrete --json"))
        XCTAssertEqual(settings.renderGpu, "discrete")
        XCTAssertEqual(settings.selectedGpu, "discrete")
    }

    func test_fr17_a_gpu_name_chosen_before_still_shows_as_chosen() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"render_gpu":"Radeon"}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.gpuChoices.last, GpuChoice(label: "Radeon", value: "Radeon"))
        XCTAssertEqual(settings.selectedGpu, "Radeon")
    }

    func test_fr17_a_refused_change_leaves_the_switch_as_the_service_keeps_it() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"freeze":false}"#)
        cli.fail("settings set freeze on --json", "dsa: the service said no")
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        await settings.setFreeze(true)
        XCTAssertFalse(settings.freeze)
        XCTAssertEqual(settings.error, "Problem_Refused (the service said no)")
    }

    func test_fr17_clearing_a_shared_folder_keeps_the_other_choices() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"file_sharing":true,"shared_folders":["Documents","Pictures"]}"#)
        cli.answer("settings set shared-folders Documents --json", #"{"shared_folders":["Documents"]}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.isFolderShared("Pictures"))
        XCTAssertFalse(settings.isFolderShared("Music"))
        await settings.setFolderShared("Pictures", false)
        XCTAssertTrue(cli.called("settings set shared-folders Documents --json"))
        XCTAssertFalse(settings.isFolderShared("Pictures"))
    }

    func test_fr17_a_switch_missing_from_an_older_service_reads_as_its_default() async {
        let (cli, service) = make()
        cli.answer("settings --json", "{}")
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.clipboard)
        XCTAssertTrue(settings.fileSharing)
        XCTAssertTrue(settings.armTranslation)
        XCTAssertEqual(settings.adbPort, 58526)
        XCTAssertEqual(settings.adbConnect, "adb connect 127.0.0.1:58526")
        XCTAssertEqual(settings.armStatus, "Arm_NotSupported")
        XCTAssertEqual(settings.armAbis, "Arm_NotRunning")
    }

    func test_fr17_an_adb_port_out_of_range_sends_nothing() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"developer_mode":true,"adb_port":58526}"#)
        cli.answer("settings set adb-port 5555 --json", #"{"developer_mode":true,"adb_port":5555}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        await settings.setAdbPort("70000")
        await settings.setAdbPort("port")
        XCTAssertFalse(cli.calls.contains { $0.contains("adb-port") })
        await settings.setAdbPort(" 5555 ")
        XCTAssertEqual(settings.adbConnect, "adb connect 127.0.0.1:5555")
    }

    func test_fr17_memory_limit_offers_automatic_the_sizes_and_one_set_elsewhere() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"memory_limit_mb":5000}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.memoryLimitChoices.map(\.mb), [0, 2048, 3072, 4096, 5000, 6144, 8192, 12288, 16384])
        XCTAssertEqual(settings.memoryLimitChoices[0].label, "MemoryLimit_Auto")
        XCTAssertEqual(settings.memoryLimitText, "5000 MB")
        await settings.setMemoryLimit(1024)
        XCTAssertFalse(cli.calls.contains { $0.contains("memory-limit") })
        cli.answer("settings set memory-limit auto --json", "{}")
        await settings.setMemoryLimit(0)
        XCTAssertEqual(settings.memoryLimitMb, 0)
    }

    func test_fr17_update_switches_are_changed_only_by_an_administrator() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"program_updates_available":true,"image_updates_available":false}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertTrue(settings.needsAdministrator)
        XCTAssertTrue(settings.programUpdatesNeedAdministrator)
        XCTAssertFalse(settings.canChangeProgramUpdates)
        XCTAssertFalse(settings.imageUpdatesNeedAdministrator)
        XCTAssertTrue(settings.canUpdateNow)
        XCTAssertTrue(settings.tracesNeedAdministrator)
        settings.isAdministrator = true
        XCTAssertTrue(settings.canChangeProgramUpdates)
        XCTAssertTrue(settings.canChangeTraces)
        XCTAssertFalse(settings.needsAdministrator)
    }

    func test_fr17_network_page_says_when_a_restart_applies_the_switch() async {
        let (cli, service) = make()
        cli.answer("settings --json", #"{"advanced_networking":true,"network":{"mode":"nat","addresses":["10.0.2.15"],"dns_servers":[],"gateway":"10.0.2.2"}}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.networkModeText, "Network_ModeNat")
        XCTAssertEqual(settings.networkAddresses, "10.0.2.15")
        XCTAssertEqual(settings.networkDns, "Network_None")
        XCTAssertEqual(settings.networkGateway, "10.0.2.2")
        XCTAssertTrue(settings.networkRestartNeeded)
        cli.answer("network --json", #"{"mode":"advanced","addresses":["192.168.1.20"]}"#)
        await settings.refreshNetwork()
        XCTAssertFalse(settings.networkRestartNeeded)
        XCTAssertEqual(settings.networkModeText, "Network_ModeAdvanced")
    }

    func test_fr17_arm_translation_status_names_the_package() async {
        let (cli, service) = make()
        cli.answer("settings --json", "{}")
        cli.answer("arm-translation status --json",
                   #"{"enabled":true,"supported":true,"package":{"name":"libndk","version":"1.0","library":"x","license":"Apache-2.0"},"android":{"installed":true,"active":true,"abilist":"x86_64,arm64-v8a"},"restart_needed":true}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        await settings.load()
        XCTAssertEqual(settings.armStatus, "Arm_Supported")
        XCTAssertTrue(settings.armRestartNeeded)
        XCTAssertTrue(settings.canRemoveArmTranslation)
    }

    func test_fr17_resetting_data_goes_through_the_service() async {
        let (cli, service) = make()
        cli.answer("reset-data --yes --json", #"{"reset":true}"#)
        let settings = SettingsViewModel(service: service, text: KeyText())
        XCTAssertEqual(settings.resetWarning, "ResetData_Warning")
        await settings.resetData()
        XCTAssertTrue(settings.resetDone)
        XCTAssertFalse(settings.isResetting)
    }

    func test_fr17_memory_page_shows_the_vm_and_the_guest_breakdown() async {
        let (cli, service) = make()
        cli.answer("memory --json", """
        {"host":[{"role":"vm","pid":10,"working_set_kb":1048576,"private_kb":2048},
                 {"role":"window manager","pid":11,"working_set_kb":10240,"private_kb":1024}],
         "host_total_kb":1058816,"vm_beyond_guest_kb":204800,
         "guest":{"mem_total_kb":2097152,"mem_available_kb":1048576,"mem_free_kb":524288,"cached_kb":102400,
                  "buffers_kb":0,"total_pss_kb":409600,"age_seconds":30,
                  "top_processes":[{"pid":400,"name":"system_server","pss_kb":102400}],
                  "kernel":{"slab_unreclaimable_kb":10240,"mem_online_kb":2097152,"cma_total_kb":0,"reserved_at_boot_kb":65536}}}
        """)
        let memory = MemoryViewModel(service: service, text: KeyText())
        await memory.refresh()
        let en = Locale(identifier: "en_US")
        XCTAssertEqual(Format.size(1048576, locale: en), "1.00 GB")
        XCTAssertEqual(memory.hostRows.map(\.label), ["Role_Vm", "Role_WindowManager"])
        XCTAssertTrue(memory.hasVmSize)
        XCTAssertTrue(memory.hasVmBeyondGuest)
        XCTAssertEqual(memory.guestUsedFraction, 0.5, accuracy: 0.0001)
        XCTAssertEqual(memory.guestRows.map(\.label).first, "Memory_Apps")
        XCTAssertEqual(memory.kernelRows.last?.label, "Memory_KernelReserved")
        XCTAssertEqual(memory.guestProcessRows.first?.label, "system_server")
        XCTAssertEqual(memory.guestAge, "Memory_ReportAge")
    }

    func test_fr17_sizes_follow_the_users_number_format() {
        XCTAssertEqual(Format.size(512, locale: Locale(identifier: "en_US")), "0.5 MB")
        XCTAssertEqual(Format.size(3 * 1024 * 1024 / 2, locale: Locale(identifier: "de_DE")), "1,50 GB")
        XCTAssertEqual(Format.fill("{0} of {1}", ["3", "7"]), "3 of 7")
    }
}
