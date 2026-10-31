//! The Android VM's HCS document.

use std::path::PathBuf;

use dsa_host_windows::guid::Guid;
use dsa_host_windows::vm::{
    hosting_process_name_suffix_supported, runtime_id_from_properties, AndroidVmConfig,
    HOSTING_PROCESS_NAME_SUFFIX,
};
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
        socket_sids: vec![USER_SID.into()],
        page_reporting_order: 5,
        console_pipe: Some(r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1".into()),
        image_version: Some("17.0.0-1".into()),
        hosting_process_name_suffix: None,
    }
}

fn document(config: &AndroidVmConfig) -> Value {
    config.to_hcs_document().expect("a valid configuration")
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
        ("outside 5 to 10", Box::new(|c| c.page_reporting_order = 12)),
    ];
    for (expected, break_it) in cases {
        let mut broken = config();
        break_it(&mut broken);
        let error = broken.to_hcs_document().unwrap_err().to_string();
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
        let error = broken.to_hcs_document().unwrap_err().to_string();
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
