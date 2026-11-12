//! The Android VM's HCS document.

use std::path::PathBuf;

use dsa_host_windows::guid::Guid;
use dsa_host_windows::vm::{
    fine_grained_memory_supported, hosting_process_name_suffix_supported, memory_resize_request,
    runtime_id_from_properties, AndroidVmConfig, GuestArch, HOSTING_PROCESS_NAME_SUFFIX,
};

#[test]
fn fr28_the_hcs_document_has_a_network_adapter_on_the_vms_endpoint() {
    use dsa_host_windows::vm::NetworkAdapter;
    let mut vm = config();
    // Without an endpoint the VM has no network adapter at all.
    assert!(document(&vm)["VirtualMachine"]["Devices"]
        .get("NetworkAdapters")
        .is_none());
    let endpoint: Guid = "6d5a0d5a-0000-4d5a-8d5a-00000000e0e0".parse().unwrap();
    vm.network_adapter = Some(NetworkAdapter {
        endpoint_id: endpoint,
        mac_address: "00-15-5D-12-34-56".into(),
    });
    for arch in [GuestArch::X86_64, GuestArch::Arm64] {
        let config = match arch {
            GuestArch::X86_64 => vm.clone(),
            GuestArch::Arm64 => {
                let mut arm = arm64_config();
                arm.network_adapter = vm.network_adapter.clone();
                arm
            }
        };
        let doc = config.to_hcs_document_for(arch).unwrap();
        let adapter = &doc["VirtualMachine"]["Devices"]["NetworkAdapters"]["eth0"];
        assert_eq!(adapter["EndpointId"], endpoint.to_string());
        assert_eq!(adapter["MacAddress"], "00-15-5D-12-34-56");
    }
}

#[test]
fn nfr2_the_memory_resize_request_names_the_vm_memory_size() {
    let request: Value = serde_json::from_str(&memory_resize_request(4609)).unwrap();
    assert_eq!(
        request["ResourcePath"],
        "VirtualMachine/ComputeTopology/Memory/SizeInMB"
    );
    assert_eq!(request["RequestType"], "Update");
    assert_eq!(request["Settings"], 4608);
}
use serde_json::Value;

const USER_SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";

fn config() -> AndroidVmConfig {
    AndroidVmConfig {
        owner: "DSA".into(),
        kernel: PathBuf::from(r"C:\Program Files\DSA\vm\kernel"),
        initrd: None,
        kernel_cmdline: "androidboot.hardware=dsa".into(),
        memory_mb: 4097,
        processors: 4,
        system_image: Some(PathBuf::from(r"C:\Program Files\DSA\vm\system.vhdx")),
        data_disk: PathBuf::from(r"C:\Users\me\AppData\Local\DSA\data.vhdx"),
        pstore_disk: Some(PathBuf::from(r"C:\Users\me\AppData\Local\DSA\pstore.vhd")),
        addon_disk: None,
        socket_sids: vec![USER_SID.into()],
        page_reporting_order: 5,
        console_pipe: Some(r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1".into()),
        image_version: Some("17.0.0-1".into()),
        hosting_process_name_suffix: None,
        fine_grained_memory: false,
        network_adapter: None,
        direct_mapped: None,
    }
}

fn document(config: &AndroidVmConfig) -> Value {
    config
        .to_hcs_document_for(GuestArch::X86_64)
        .expect("a valid configuration")
}

const BOOT_FOLDER: &str = r"C:\ProgramData\DSA\tmp\dsa-android-7-0";

/// An ARM64 VM's configuration: the kernel and the initrd in one boot folder, as the launcher
/// lays them out.
fn arm64_config() -> AndroidVmConfig {
    let mut config = config();
    let folder = PathBuf::from(BOOT_FOLDER);
    config.kernel = folder.join("Image");
    config.initrd = Some(folder.join("initrd.img"));
    config
}

fn arm64_document(config: &AndroidVmConfig) -> Value {
    config
        .to_hcs_document_for(GuestArch::Arm64)
        .expect("a valid ARM64 configuration")
}

#[test]
fn fr27_the_host_architecture_picks_the_guest_architecture() {
    let expected = if cfg!(target_arch = "aarch64") {
        GuestArch::Arm64
    } else {
        GuestArch::X86_64
    };
    assert_eq!(GuestArch::HOST, expected);
    let config = if GuestArch::HOST == GuestArch::Arm64 {
        arm64_config()
    } else {
        config()
    };
    assert_eq!(
        config.to_hcs_document().unwrap(),
        config.to_hcs_document_for(GuestArch::HOST).unwrap()
    );
    assert_eq!(
        config.full_kernel_cmdline(),
        config.full_kernel_cmdline_for(GuestArch::HOST)
    );
}

#[test]
fn fr27_an_arm64_vm_boots_the_kernels_efi_stub_from_its_boot_folder() {
    let doc = arm64_document(&arm64_config());
    let chipset = &doc["VirtualMachine"]["Chipset"];
    assert!(
        chipset.get("LinuxKernelDirect").is_none(),
        "Hyper-V on ARM64 has no direct Linux boot"
    );
    assert_eq!(chipset["UseUtc"], true);
    let entry = &chipset["Uefi"]["BootThis"];
    assert_eq!(entry["DeviceType"], "VmbFs");
    assert_eq!(entry["VmbFsRootPath"], BOOT_FOLDER);
    assert_eq!(entry["DevicePath"], r"\Image");
    let options = entry["OptionalData"].as_str().unwrap();
    assert!(
        options.starts_with(
            "initrd=\\initrd.img earlycon=pl011,0xeffec000,115200 console=ttyAMA0,115200 \
             panic=-1 page_reporting.page_reporting_order=5"
        ),
        "{options}"
    );
    assert!(options.ends_with("androidboot.hardware=dsa"), "{options}");
    assert_eq!(
        entry.as_object().unwrap().len(),
        4,
        "only the fields HCS knows: {entry}"
    );
}

#[test]
fn fr27_an_arm64_kernel_gets_no_x86_parameters() {
    let options = arm64_config().full_kernel_cmdline_for(GuestArch::Arm64);
    for x86 in ["uart8250", "ttyS0", "memmap=", "ramoops.mem_address"] {
        assert!(!options.contains(x86), "{x86} in {options}");
    }
    // The last log lives on the pstore disk on both architectures, so no RAM is reserved.
    assert!(!options.contains("ramoops"), "{options}");
    let x86 = config().full_kernel_cmdline_for(GuestArch::X86_64);
    for arm in ["pl011", "ttyAMA0", "reserve_mem="] {
        assert!(!x86.contains(arm), "{arm} in {x86}");
    }
}

#[test]
fn fr27_an_arm64_vm_keeps_the_same_devices_memory_and_console_pipe() {
    let arm = arm64_document(&arm64_config());
    let x86 = document(&arm64_config());
    for key in ["ComputeTopology", "Devices"] {
        let (arm, x86) = (&arm["VirtualMachine"][key], &x86["VirtualMachine"][key]);
        assert_eq!(arm, x86, "{key}");
    }
    assert_eq!(
        arm["VirtualMachine"]["Devices"]["ComPorts"]["0"]["NamedPipe"],
        r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1"
    );
    assert_eq!(arm["VirtualMachine"]["StopOnReset"], true);
    assert_eq!(arm["SchemaVersion"]["Minor"], 3);
}

#[test]
fn fr27_an_arm64_vm_without_an_initrd_names_none() {
    let mut config = arm64_config();
    config.initrd = None;
    let doc = arm64_document(&config);
    let options = doc["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"]["OptionalData"]
        .as_str()
        .unwrap();
    assert!(options.starts_with("earlycon=pl011"), "{options}");
}

#[test]
fn fr27_an_arm64_initrd_outside_the_kernels_folder_is_refused() {
    // The firmware reads only the boot folder, so it could not load the initrd.
    let mut outside = config();
    outside.initrd = Some(PathBuf::from(r"C:\ProgramData\DSA\initrd.img"));
    let error = outside
        .to_hcs_document_for(GuestArch::Arm64)
        .unwrap_err()
        .to_string();
    assert!(error.contains("kernel's folder"), "{error}");
    // x86_64 boots the files from wherever they are.
    document(&outside);
}

#[test]
fn fr2_the_vm_boots_its_kernel_directly_with_the_android_command_line() {
    let doc = document(&config());
    let kernel = &doc["VirtualMachine"]["Chipset"]["LinuxKernelDirect"];
    assert_eq!(kernel["KernelFilePath"], r"C:\Program Files\DSA\vm\kernel");
    assert!(
        kernel.get("InitRdPath").is_none(),
        "no initrd was configured"
    );
    let cmdline = kernel["KernelCmdLine"].as_str().unwrap();
    assert!(
        cmdline.starts_with(
            "earlycon=uart8250,io,0x3f8,115200 console=ttyS0,115200 panic=-1 \
             page_reporting.page_reporting_order=5"
        ),
        "{cmdline}"
    );
    assert!(!cmdline.contains("hvc0"), "{cmdline}");
    assert!(cmdline.ends_with("androidboot.hardware=dsa"), "{cmdline}");
    assert_eq!(doc["SchemaVersion"]["Major"], 2);
    assert_eq!(doc["ShouldTerminateOnLastHandleClosed"], true);
}

#[test]
fn nfr2_the_vm_faults_and_frees_memory_in_small_pieces_where_the_build_can() {
    let mut fine = config();
    fine.fine_grained_memory = true;
    let doc = document(&fine);
    let memory = &doc["VirtualMachine"]["ComputeTopology"]["Memory"];
    assert_eq!(memory["BackingPageSize"], "Small");
    assert_eq!(memory["FaultClusterSizeShift"], 4);
    assert_eq!(memory["DirectMapFaultClusterSizeShift"], 4);

    let doc = document(&config());
    let memory = &doc["VirtualMachine"]["ComputeTopology"]["Memory"];
    assert_eq!(memory.get("BackingPageSize"), None);
    assert_eq!(memory.get("FaultClusterSizeShift"), None);
    assert_eq!(memory.get("DirectMapFaultClusterSizeShift"), None);
}

#[test]
fn nfr2_fine_grained_memory_is_only_used_on_builds_with_the_teardown_fix() {
    assert!(fine_grained_memory_supported(26100, 0));
    assert!(fine_grained_memory_supported(26300, 0));
    assert!(fine_grained_memory_supported(22000, 2360));
    assert!(fine_grained_memory_supported(22631, 2360));
    assert!(!fine_grained_memory_supported(22000, 1969));
    assert!(fine_grained_memory_supported(20348, 1970));
    assert!(!fine_grained_memory_supported(20348, 1969));
    assert!(fine_grained_memory_supported(19045, 3393));
    assert!(!fine_grained_memory_supported(19045, 3392));
    assert!(!fine_grained_memory_supported(19041, 9999));
}

#[test]
fn nfr2_the_vm_memory_is_set_up_to_return_to_the_host() {
    let doc = document(&config());
    let memory = &doc["VirtualMachine"]["ComputeTopology"]["Memory"];
    assert_eq!(memory["AllowOvercommit"], true);
    assert_eq!(memory["EnableDeferredCommit"], true);
    assert_eq!(memory["EnableColdDiscardHint"], true);
    assert_eq!(memory["EnableColdHint"], true);
    assert_eq!(memory.get("EnableHotHint"), None);
    assert_eq!(
        memory["SizeInMB"], 4096,
        "rounded down to the 2 MiB granularity"
    );
    assert_eq!(
        doc["VirtualMachine"]["ComputeTopology"]["Processor"]["Count"],
        4
    );
}

#[test]
fn fr2_the_system_image_is_read_only_and_the_data_disk_writable_and_sparse() {
    let doc = document(&config());
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(disks["0"]["Path"], r"C:\Program Files\DSA\vm\system.vhdx");
    assert_eq!(disks["0"]["ReadOnly"], true);
    assert_eq!(
        disks["1"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\data.vhdx"
    );
    assert_eq!(disks["1"]["ReadOnly"], false);
    assert_eq!(disks["1"]["AlwaysAllowSparseFiles"], true);
}

#[test]
fn fr21_the_pstore_disk_is_attached_writable_after_the_other_disks() {
    let doc = document(&config());
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        disks["2"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\pstore.vhd"
    );
    assert_eq!(disks["2"]["ReadOnly"], false);
    assert_eq!(disks["2"]["Type"], "VirtualDisk");

    let mut one_disk = config();
    one_disk.system_image = None;
    let doc = document(&one_disk);
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        disks["0"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\data.vhdx"
    );
    assert_eq!(
        disks["1"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\pstore.vhd"
    );
}

#[test]
fn fr32_the_addon_disk_is_attached_read_only_after_the_pstore_disk() {
    let mut with_addons = config();
    with_addons.addon_disk = Some(PathBuf::from(r"C:\Users\me\AppData\Local\DSA\addons.vhd"));
    let doc = document(&with_addons);
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        disks["3"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\addons.vhd"
    );
    assert_eq!(disks["3"]["ReadOnly"], true);
    assert_eq!(disks["3"]["Type"], "VirtualDisk");
    // Without add-ons there is no fourth disk.
    let doc = document(&config());
    assert!(doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"]["3"].is_null());
    assert_eq!(
        dsa_host_windows::paths::addon_disk_beside(std::path::Path::new(
            r"C:\Users\me\AppData\Local\DSA\android.vhdx"
        )),
        PathBuf::from(r"C:\Users\me\AppData\Local\DSA\addons.vhd")
    );
}

#[test]
fn fr21_the_kernel_keeps_its_last_log_on_the_pstore_disk_not_in_ram() {
    let doc = document(&config());
    let cmdline = doc["VirtualMachine"]["Chipset"]["LinuxKernelDirect"]["KernelCmdLine"]
        .as_str()
        .unwrap();
    assert!(
        cmdline.contains(&format!(
            "pstore_blk.blkdev=PARTUUID={}",
            dsa_host_windows::pstore_disk::PARTITION_UUID
        )),
        "{cmdline}"
    );
    assert!(cmdline.contains("pstore_blk.best_effort=y"), "{cmdline}");
    // A panic ends the VM, so a RAM region would keep nothing and only cost memory.
    assert!(!cmdline.contains("ramoops"), "{cmdline}");
    assert!(!cmdline.contains("memmap="), "{cmdline}");

    let mut without = config();
    without.pstore_disk = None;
    let doc = document(&without);
    let cmdline = doc["VirtualMachine"]["Chipset"]["LinuxKernelDirect"]["KernelCmdLine"]
        .as_str()
        .unwrap();
    assert!(!cmdline.contains("pstore_blk"), "{cmdline}");
    assert!(doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"]["2"].is_null());
}

#[test]
fn pr1_only_system_and_the_vms_user_may_use_its_hyper_v_sockets() {
    let doc = document(&config());
    let sockets = &doc["VirtualMachine"]["Devices"]["HvSocket"]["HvSocketConfig"];
    let expected = format!("D:P(A;;FA;;;SY)(A;;FA;;;{USER_SID})");
    assert_eq!(sockets["DefaultBindSecurityDescriptor"], expected.as_str());
    assert_eq!(
        sockets["DefaultConnectSecurityDescriptor"],
        expected.as_str()
    );
}

#[test]
fn fr3_default_bind_security_descriptor_grants_user_sid_bind_access() {
    let mut cfg = config();
    let second_sid = "S-1-5-21-999999999-888888888-777777777-1002";
    cfg.socket_sids.push(second_sid.to_string());
    let doc = document(&cfg);
    let sockets = &doc["VirtualMachine"]["Devices"]["HvSocket"]["HvSocketConfig"];
    let bind_sddl = sockets["DefaultBindSecurityDescriptor"]
        .as_str()
        .expect("DefaultBindSecurityDescriptor string");

    // The bind security descriptor must grant the user full access so user-session listeners can bind.
    assert!(bind_sddl.contains(&format!("(A;;FA;;;{USER_SID})")));
    assert!(bind_sddl.contains(&format!("(A;;FA;;;{second_sid})")));
    assert!(bind_sddl.contains("(A;;FA;;;SY)"));
}

#[test]
fn fr2_an_initrd_is_passed_when_configured() {
    let mut with_initrd = config();
    with_initrd.initrd = Some(PathBuf::from(r"C:\Program Files\DSA\vm\initrd.img"));
    let doc = document(&with_initrd);
    assert_eq!(
        doc["VirtualMachine"]["Chipset"]["LinuxKernelDirect"]["InitRdPath"],
        r"C:\Program Files\DSA\vm\initrd.img"
    );
}

/// One way to break a valid configuration.
type Breakage = Box<dyn Fn(&mut AndroidVmConfig)>;

#[test]
fn fr2_configurations_that_cannot_make_a_vm_are_refused_with_the_reason() {
    let cases: Vec<(&str, Breakage)> = vec![
        (
            "must be absolute",
            Box::new(|c| c.kernel = PathBuf::from("kernel")),
        ),
        (
            "must be absolute",
            Box::new(|c| c.data_disk = PathBuf::from(r"data\disk.vhdx")),
        ),
        ("too little", Box::new(|c| c.memory_mb = 256)),
        ("at least one processor", Box::new(|c| c.processors = 0)),
        (
            "not a SID",
            Box::new(|c| c.socket_sids = vec!["S-1-5-x)(A;;FA;;;WD".into()]),
        ),
        ("outside 0 to 10", Box::new(|c| c.page_reporting_order = 12)),
    ];
    for (expected, break_it) in cases {
        let mut broken = config();
        break_it(&mut broken);
        let error = broken
            .to_hcs_document_for(GuestArch::X86_64)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn fr2_the_runtime_id_is_read_from_the_vm_properties() {
    let id = "1c4f0e0a-6b2d-4a7e-9f3b-2d5c8e7a9b10";
    let expected: Guid = id.parse().unwrap();
    for document in [
        format!(r#"{{"Id":"dsa","RuntimeId":"{id}","State":"Running"}}"#),
        format!(r#"{{"RuntimeID":"{{{id}}}"}}"#),
    ] {
        assert_eq!(
            runtime_id_from_properties(&document),
            Some(expected),
            "{document}"
        );
    }
    assert_eq!(runtime_id_from_properties("not json"), None);
    assert_eq!(runtime_id_from_properties(r#"{"RuntimeId":"nope"}"#), None);
}

#[test]
fn fr2_the_guest_console_goes_to_a_named_pipe_on_the_first_serial_port() {
    let doc = document(&config());
    let ports = &doc["VirtualMachine"]["Devices"]["ComPorts"];
    assert_eq!(
        ports,
        &serde_json::json!({ "0": { "NamedPipe": r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1" } })
    );
    let cmdline = doc["VirtualMachine"]["Chipset"]["LinuxKernelDirect"]["KernelCmdLine"]
        .as_str()
        .unwrap();
    assert!(
        cmdline.contains("earlycon=uart8250,io,0x3f8,115200"),
        "COM1 from the first instruction: {cmdline}"
    );
    assert!(cmdline.contains("console=ttyS0,115200"), "{cmdline}");
    // The other devices are still there.
    assert!(doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"]["1"].is_object());
    assert!(doc["VirtualMachine"]["Devices"]["HvSocket"].is_object());
}

#[test]
fn fr2_each_vm_has_its_own_console_pipe() {
    use dsa_host_windows::vm::console_pipe_path;
    assert_eq!(
        console_pipe_path("dsa-android-S-1-5-21-1-2-3-1001"),
        r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1-2-3-1001"
    );
    assert_ne!(console_pipe_path("a"), console_pipe_path("b"));
}

#[test]
fn fr2_without_a_console_pipe_the_vm_has_no_serial_port() {
    let mut without = config();
    without.console_pipe = None;
    let doc = document(&without);
    assert!(doc["VirtualMachine"]["Devices"].get("ComPorts").is_none());
}

#[test]
fn fr2_a_console_pipe_that_is_not_a_local_pipe_is_refused() {
    for bad in [
        r"C:\console",
        r"\\.\pipe\",
        r"\\server\pipe\x",
        r#"\\.\pipe\a"b"#,
    ] {
        let mut broken = config();
        broken.console_pipe = Some(bad.into());
        let error = broken
            .to_hcs_document_for(GuestArch::X86_64)
            .unwrap_err()
            .to_string();
        assert!(error.contains("console pipe"), "{bad:?}: {error}");
    }
}

#[test]
fn the_memory_process_is_named_vmmem_dsa_when_a_suffix_is_set() {
    let mut config = config();
    config.hosting_process_name_suffix = Some(HOSTING_PROCESS_NAME_SUFFIX.into());
    let doc = document(&config);
    let memory = &doc["VirtualMachine"]["ComputeTopology"]["Memory"];
    assert_eq!(memory["HostingProcessNameSuffix"], "DSA");
}

#[test]
fn the_memory_process_keeps_its_plain_name_without_a_suffix() {
    let doc = document(&config());
    let memory = &doc["VirtualMachine"]["ComputeTopology"]["Memory"];
    assert!(
        memory.get("HostingProcessNameSuffix").is_none(),
        "a build that does not know the field rejects the whole document"
    );
}

#[test]
fn the_memory_process_suffix_is_only_used_where_hcs_accepts_it() {
    assert!(!hosting_process_name_suffix_supported(19045, 5000));
    assert!(!hosting_process_name_suffix_supported(22000, 70));
    assert!(hosting_process_name_suffix_supported(22000, 71));
    assert!(hosting_process_name_suffix_supported(22138, 0));
    assert!(!hosting_process_name_suffix_supported(22350, 0));
    assert!(!hosting_process_name_suffix_supported(22419, 0));
    assert!(hosting_process_name_suffix_supported(22420, 0));
    assert!(hosting_process_name_suffix_supported(26100, 0));
}

#[test]
fn nfr2_the_guest_kernel_sets_little_memory_aside_at_boot() {
    for arch in [GuestArch::X86_64, GuestArch::Arm64] {
        let cmdline = if arch == GuestArch::Arm64 {
            arm64_config().full_kernel_cmdline_for(arch)
        } else {
            config().full_kernel_cmdline_for(arch)
        };
        let words: Vec<&str> = cmdline.split_whitespace().collect();
        // A 4 MiB bounce buffer, not the 64 MiB default, which the VMBus devices never use.
        assert!(words.contains(&"swiotlb=2048"), "{cmdline}");
        // No KFENCE pool, and hash tables sized for a desktop guest rather than its memory.
        for word in [
            "kfence.sample_interval=0",
            "dhash_entries=65536",
            "ihash_entries=32768",
            "thash_entries=16384",
            "uhash_entries=1024",
        ] {
            assert!(words.contains(&word), "{word} in {cmdline}");
        }
        // Nothing reserves RAM for a crash kernel or a RAM log.
        for reserving in ["crashkernel=", "ramoops.mem_size", "memmap=", "cma="] {
            assert!(!cmdline.contains(reserving), "{reserving} in {cmdline}");
        }
        // The user's own parameters still come last, so they can override these.
        assert!(cmdline.ends_with("androidboot.hardware=dsa"), "{cmdline}");
    }
}

fn direct_mapped(addon_bytes: Option<u64>) -> dsa_host_windows::vm::DirectMapped {
    dsa_host_windows::vm::DirectMapped {
        system_disk: PathBuf::from(r"C:\ProgramData\DSA\direct-mapped\17.0.0-1.vhd"),
        system_bytes: 1800 << 20,
        addon_bytes,
        support: dsa_host_core::direct_mapped::ImageSupport {
            fstab_suffix: "dsa_dax".into(),
            system_disk: dsa_host_core::direct_mapped_disk::SystemDisk {
                format: dsa_host_core::direct_mapped_disk::FORMAT,
                bytes: 1800 << 20,
                sha256: "ab".repeat(32),
                super_bytes: 1600 << 20,
            },
            verified_partitions: vec!["odm".into()],
        },
    }
}

#[test]
fn nfr1_a_direct_mapped_vm_maps_its_read_only_disks_as_persistent_memory() {
    let mut vm = config();
    vm.addon_disk = Some(PathBuf::from(r"C:\Users\me\AppData\Local\DSA\addons.vhd"));
    vm.kernel_cmdline =
        "androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa androidboot.hardware=dsa"
            .into();
    vm.direct_mapped = Some(direct_mapped(Some(300 << 20)));
    let doc = document(&vm);
    let devices = &doc["VirtualMachine"]["Devices"];
    // The system disk and the add-on disk are read-only persistent memory, mapped from their
    // flat VHD files, and no longer SCSI disks; the user's disk and the pstore disk stay there.
    let pmem = &devices["VirtualPMem"];
    assert_eq!(
        pmem["Devices"]["0"]["HostPath"],
        r"C:\ProgramData\DSA\direct-mapped\17.0.0-1.vhd"
    );
    assert_eq!(
        pmem["Devices"]["1"]["HostPath"],
        r"C:\Users\me\AppData\Local\DSA\addons.vhd"
    );
    for device in ["0", "1"] {
        assert_eq!(pmem["Devices"][device]["ReadOnly"], true, "{device}");
        assert_eq!(pmem["Devices"][device]["ImageFormat"], "Vhd1", "{device}");
    }
    assert_eq!(pmem["MaximumCount"], 2, "no empty devices");
    assert_eq!(pmem["MaximumSizeBytes"], 1800u64 << 20);
    // Backed by a mapping of the file, which the host can drop and read again, never by
    // memory of the VM's own.
    assert_eq!(pmem["Backing"], "Virtual");
    let disks = devices["Scsi"]["0"]["Attachments"].as_object().unwrap();
    let mut paths: Vec<&str> = disks
        .values()
        .map(|d| d["Path"].as_str().unwrap())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            r"C:\Users\me\AppData\Local\DSA\data.vhdx",
            r"C:\Users\me\AppData\Local\DSA\pstore.vhd"
        ]
    );
    assert_eq!(disks["0"]["ReadOnly"], false);
    // The guest boots the image's mount table for the mode, with the persistent memory's
    // platform device among the boot devices, each named once.
    let cmdline = vm.full_kernel_cmdline_for(GuestArch::X86_64);
    assert!(
        cmdline.contains(" androidboot.boot_devices=vmbus,nfit "),
        "{cmdline}"
    );
    assert!(
        cmdline.contains(" androidboot.fstab_suffix=dsa_dax "),
        "{cmdline}"
    );
    assert_eq!(cmdline.matches("androidboot.fstab_suffix=").count(), 1);
    assert!(cmdline.ends_with("androidboot.hardware=dsa"), "{cmdline}");
}

#[test]
fn nfr1_without_the_direct_mapped_mode_the_disks_stay_on_scsi() {
    let mut vm = config();
    vm.addon_disk = Some(PathBuf::from(r"C:\Users\me\AppData\Local\DSA\addons.vhd"));
    let doc = document(&vm);
    assert!(doc["VirtualMachine"]["Devices"]
        .get("VirtualPMem")
        .is_none());
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(disks["0"]["Path"], r"C:\Program Files\DSA\vm\system.vhdx");
    assert_eq!(
        disks["3"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\addons.vhd"
    );
    assert!(!vm
        .full_kernel_cmdline_for(GuestArch::X86_64)
        .contains("dsa_dax"));
    // A direct-mapped boot with no add-on disk to map keeps an attached one on SCSI.
    vm.direct_mapped = Some(direct_mapped(None));
    let doc = document(&vm);
    let pmem = &doc["VirtualMachine"]["Devices"]["VirtualPMem"];
    assert_eq!(pmem["MaximumCount"], 1);
    assert!(pmem["Devices"].get("1").is_none());
    let disks = &doc["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        disks["2"]["Path"],
        r"C:\Users\me\AppData\Local\DSA\addons.vhd"
    );
}

#[test]
fn nfr1_a_direct_mapped_vm_that_could_not_map_its_disks_is_refused() {
    let mut vm = config();
    let mut mapped = direct_mapped(None);
    mapped.system_bytes = (1800 << 20) + 4096;
    vm.direct_mapped = Some(mapped);
    let why = vm
        .to_hcs_document_for(GuestArch::X86_64)
        .unwrap_err()
        .to_string();
    assert!(why.contains("whole number of 2 MiB"), "{why}");
    let mut arm = arm64_config();
    arm.direct_mapped = Some(direct_mapped(None));
    let why = arm
        .to_hcs_document_for(GuestArch::Arm64)
        .unwrap_err()
        .to_string();
    assert!(why.contains("x86_64"), "{why}");
}
