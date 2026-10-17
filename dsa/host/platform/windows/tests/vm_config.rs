//! The Android VM's HCS document.

use std::path::PathBuf;

use dsa_host_windows::guid::Guid;
use dsa_host_windows::vm::{runtime_id_from_properties, AndroidVmConfig};
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
        system_image: PathBuf::from(r"C:\Program Files\DSA\vm\system.vhdx"),
        data_disk: PathBuf::from(r"C:\Users\me\AppData\Local\DSA\data.vhdx"),
        socket_sids: vec![USER_SID.into()],
        page_reporting_order: 5,
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
        cmdline.starts_with("console=hvc0 panic=-1 page_reporting.page_reporting_order=5"),
        "{cmdline}"
    );
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
