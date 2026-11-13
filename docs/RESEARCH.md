# RESEARCH

이 문서는 결정을 내리기 위해 한 조사를 기록한다. 조사에 쓴 스크립트와 실험 폴더는 남기지 않고, 무엇을 확인했고 무엇을 결정했는지만 남긴다. 결정 자체는 INTENT에 있다.

## R1. stock WSL 커널에 binder를 외부 모듈로 올릴 수 있는가

질문: WSL이 배포하는 커널을 바꾸지 않고, binder(binderfs 포함)를 외부 모듈로 올릴 수 있는가.

답(2026-10-16): **upstream 드라이버로는 안 된다.** 드라이버가 쓰는 커널 심볼 가운데 stock WSL 커널이 모듈에 내보내지 않는 것이 있다. 드라이버를 고친 사본만 올라가고, 고치는 방법마다 비용이 있다.

### 확인한 버전

| 대상 | 버전 |
|---|---|
| WSL 커널(현재) | `linux-msft-wsl-6.18.54.1` (커밋 `d504d40a`) |
| WSL 커널(이전 계열) | `linux-msft-wsl-6.6.123.2` (커밋 `a07f9ea8`) |
| WSL | `microsoft/WSL` `master` `9a32b5ec` (2026-10-15). `packages.config`가 `Microsoft.WSL.Kernel` 6.18.54.1을 고정한다 |
| `.wslconfig` 문서 | MicrosoftDocs/WSL `WSL/wsl-config.md` (2026-10-16 확인) |

### 확인 방법

해당 태그를 받아 `Microsoft/config-wsl`로 `make modules_prepare`만 돌렸다(커널 전체 빌드는 하지 않았다). 그다음 `drivers/android/`의 소스를 트리 밖 모듈로 컴파일하고, `nm -u`로 필요한 심볼을 뽑아 트리 안에 `EXPORT_SYMBOL*`이 있는지 찾았다. `make LOCALVERSION=`을 줘야 git 체크아웃에서 release 문자열 끝에 `+`가 붙지 않아 vermagic이 맞는다. 표의 심볼은 모두 손으로 다시 확인했다. 정의는 있고 export는 없다.

### 결과

1. **binder는 stock 커널에 없고 모듈로 고를 수도 없다.** `config-wsl`에 `# CONFIG_ANDROID_BINDER_IPC is not set`이 있다(두 태그 모두). upstream `Kconfig`는 `ANDROID_BINDER_IPC`, `ANDROID_BINDERFS`, `ANDROID_BINDER_IPC_RUST`를 `tristate`가 아닌 `bool`로 선언한다. 그래서 "모듈로 빌드"는 언제나 소스 사본을 트리 밖에서 빌드한다는 뜻이다.
2. **`.wslconfig`의 `kernelModules`는 stock 커널과 함께 쓸 수 없다.** `kernel` 없이 `kernelModules`만 주면 WSL이 VM을 시작하지 않는다(`WslCoreVm.cpp`의 `MessageMismatchedKernelModulesError`, "Customised kernel modules were supplied without specifying a customised kernel."). stock 커널에서 남는 길은 VM이 뜬 뒤 배포판 안에서 `insmod`하는 것뿐이다.
3. **서명과 vermagic은 막지 않는다.** `CONFIG_MODULE_SIG`가 꺼져 있다. `CONFIG_MODVERSIONS=y`라 심볼마다 CRC가 맞아야 하지만, 같은 태그와 설정으로 빌드하면 맞는다. 대신 모듈 하나는 커널 release 하나에만 맞는다. WSL이 커널을 올릴 때마다 새로 빌드해야 한다.
4. **upstream 드라이버는 export되지 않은 심볼을 쓴다.**

   | 심볼 | 6.6.123.2 | 6.18.54.1 | binder가 쓰는 곳 |
   |---|---|---|---|
   | `security_binder_set_context_mgr`, `security_binder_transaction`, `security_binder_transfer_binder`, `security_binder_transfer_file` | 없음 | 없음 | LSM(SELinux) 검사 |
   | `task_work_add` | 없음 | 없음 | 받은 fd의 지연 close |
   | `close_fd_get_file` / `file_close_fd` | 없음 | 없음 | 위 지연 close에서 fd 표에서 파일을 꺼냄(6.8에서 이름이 바뀜) |
   | `can_nice` | 없음 | 없음 | 트랜잭션의 우선순위 상속 |
   | `__wake_up_pollfree` | 없음 | 없음 | 스레드 종료 시 epoll 대기자 깨우기 |
   | `init_ipc_ns`, `put_ipc_ns` | 없음 | 없음 | IPC 네임스페이스마다 binderfs 하나 |
   | `zap_page_range_single` | 없음 | 없음 | shrinker가 놓은 버퍼 페이지 unmap |
   | `list_lru_add`, `list_lru_del` | 있음 | 없음 | 빈 페이지 LRU(6.18은 `_obj` 변형만 export) |
   | `lock_vma_under_rcu` | 안 씀 | 없음 | shrinker의 lock-free VMA 조회 |

   6.6에서 6.18로 가며 빠진 심볼이 11개에서 14개로 늘었다. 이것이 유지 비용의 축소판이다.

   흔한 우회(실행 중에 주소를 찾기)에는 두 가지가 걸린다. `CONFIG_KPROBES=y`, `CONFIG_KALLSYMS_ALL=y`라 kprobe로 `kallsyms_lookup_name`을 찾을 수는 있다. 하지만 `CONFIG_X86_KERNEL_IBT=y`라, 커널 안에서 주소가 쓰이지 않는 함수는 objtool이 `ENDBR`를 봉인한다. 그런 함수를 찾은 포인터로 간접 호출하면 fault가 날 수 있다. IBT가 실제로 켜지는지는 CPU와 Hyper-V가 CET를 넘기는지에 달려 있어 실제 장비에서만 확인할 수 있다.

   ashmem은 관계없다. binder와 binderfs는 ashmem이 필요 없고 `CONFIG_MEMFD_CREATE=y`다.

### 선택지와 결정

| | 선택지 | 비용 | 얻는 것 |
|---|---|---|---|
| A | 고친 트리 밖 binder, export 안 된 심볼은 kprobe와 kallsyms로 실행 중에 찾음 | 커널 계열마다 binder fork, WSL 커널 release마다 빌드, VM 전체에 `ibt=off`가 필요할 수 있음, 내부 심볼이 예고 없이 바뀜 | stock 커널 유지, 사용자는 빌드하지 않음 |
| B | 고친 트리 밖 binder, export 안 된 호출을 export된 것으로 바꾸거나 뺌 | A와 같은 fork와 빌드. LSM 검사가 빠지고(binder에 SELinux 검사 없음), epoll 깨우기와 우선순위 상속은 근사치가 됨 | 내부 심볼 조회와 IBT 문제가 없음 |
| C | binder를 넣은 자체 커널을 `.wslconfig` `kernel`로 설치 | 사용자의 모든 WSL 배포판에 영향, WSL 커널 release를 직접 따라가야 함, 설정해 둔 동안 WSL 자체 커널 업데이트가 끊김 | upstream binder 그대로 |
| D | Microsoft에 `config-wsl`에서 binder를 켜 달라고 요청 | 시간이 걸리고 답은 우리 몫이 아님 | 문제 자체가 없어짐 |

**결정(소유자, 2026-10-16):** A부터 C 가운데 어느 것도 기본 경로가 되지 않는다. A와 B는 release마다 바뀌는 WSL 커널 내부에 기대고, C는 사용자의 모든 WSL 배포판을 바꾼다. 안드로이드는 binder를 넣은 자체 커널로 전용 경량 VM에서 돌린다(R2, INTENT D1과 D2). D는 함께 진행한다. 아래 부록 A가 이슈 초안이다. WSL 커널에 binder가 들어오면 VM 옆에 WSL 컨테이너 백엔드를 더할 수 있다.

### 확인하지 못한 것

- 실제 WSL VM에 모듈을 올려 보는 것과 IBT 확인. 조사 환경에 WSL과 Hyper-V가 없었다. 결과는 소스와 컴파일에서 나왔다.
- 커널 전체 빌드. 전체 빌드에서는 modpost가 `Module.symvers`로 심볼을 푼다. 여기서는 소스 검색으로 대신했다.

## R2. 안드로이드를 전용 경량 VM에서 돌릴 수 있는가

질문: binder를 stock WSL 커널에 올릴 수 없고 WSL 커널을 바꾸면 모든 배포판이 바뀐다. 그렇다면 binder를 넣은 자체 커널로 안드로이드만의 경량 VM을 돌리면서, WSA를 무겁게 만든 비용을 다시 들이지 않을 수 있는가.

답(2026-10-16): **WSL이 쓰는 것과 같은 Windows 기능으로 된다.** WSL을 가볍게 하는 요소(호스트로 메모리 반환, sparse 디스크, Hyper-V 소켓, 커널 직접 부팅)는 모두 Host Compute System(HCS) API의 공개된 옵션이다. WSL 전용이 아니다. 대가는 권한 있는 서비스가 필요하다는 점이다. HCS로 VM을 만들 수 있는 것은 관리자뿐이다.

### 근거로 삼은 자료

| 자료 | 버전 |
|---|---|
| HCS 문서, MicrosoftDocs/Virtualization-Documentation `virtualization/api/hcs/` | 커밋 `7db63dd9` (2026-10-07) |
| WSL 소스, `microsoft/WSL` (MIT) | `master` `9a32b5ec` (2026-10-15) |
| WSL 커널 설정, `Microsoft/config-wsl` | `linux-msft-wsl-6.18.54.1` |

### 결과

1. **HCS는 공개 API이고 WSL이 쓰는 것이다.** `computecore.dll`의 `HcsCreateComputeSystem`은 Windows 10 1809부터 문서화돼 있다. WSL은 `WslCoreVm::GenerateConfigJson`이 만든 JSON으로 같은 함수를 부른다. WSL 2는 Virtual Machine Platform만 켠 Windows 11 Home에서도 돈다. 그러니 WSL 2가 도는 곳에는 HCS가 있다. 이것은 두 사실에서 낸 추론이고 문서로 확인한 것은 아니다.
2. **VM은 관리자만 만들 수 있어서 서비스가 필요하다.** `HCS_E_ACCESS_DENIED`: 관리자나 Hyper-V Administrators 그룹만 VM에 접근할 수 있다. WSL도 SYSTEM으로 도는 `wslservice`가 사용자 대신 VM을 만든다. DSA도 같은 구조로, 설치 프로그램이 서비스를 등록하고 사용자 세션의 프로세스가 그 서비스와 이야기한다.
3. **메모리는 WSL과 같은 방식으로 호스트에 돌아간다.** WSL은 `Memory.AllowOvercommit`(페이지 파일로 뒷받침, 고정 할당 아님), `EnableDeferredCommit`(처음 쓸 때 commit), `EnableColdDiscardHint`(게스트가 빈 페이지를 돌려줌), 새 빌드에서는 `BackingPageSize = Small`을 켜고, 커널 명령줄에 `page_reporting.page_reporting_order=5` 또는 `9`를 넣는다. 게스트 쪽에는 `CONFIG_HYPERV_BALLOON`과 `CONFIG_PAGE_REPORTING`이 필요하다. WSA가 메모리를 놓지 않은 것은 VM이라서가 아니다.
4. **커널을 직접 부팅한다.** `Chipset.LinuxKernelDirect`가 `KernelFilePath`, `InitRdPath`, `KernelCmdLine`을 받는다. x64에서는 WSL도 이것을 쓴다. ARM64에서는 WSL이 UEFI로 부팅한다("Linux kernel direct boot is not yet supported on ARM64"). 나중에 ARM용 Windows를 지원할 때 걸리는 점이다. 자체 커널이라 binder가 들어가 있고, 안드로이드 init이 첫 프로세스가 된다. 리눅스 배포판도 LXC도 사이에 없다.
5. **Hyper-V 소켓은 VM마다 설정한다.** `HvSocket` 장치가 기본 bind/connect 보안 설명자와 서비스별 `ServiceTable`을 받는다. VM을 우리가 만들기 때문에 VM ID도 우리가 안다. WSL 경로에서 열려 있던 "WSL VM의 ID를 어떻게 알아내나" 문제가 없어진다.
6. **디스크: 읽기 전용 시스템 이미지와 sparse 데이터 디스크.** SCSI 연결이 `ReadOnly`와 `AlwaysAllowSparseFiles`를 받는다. 시스템 이미지는 업데이트가 통째로 바꾸는 읽기 전용 파일로, 사용자 데이터는 게스트에서 discard로 줄어드는 sparse VHDX로 둔다. 커지기만 하던 WSA 데이터 디스크 문제는 이것으로 다룬다. 실제 장비에서 재야 한다.
7. **호스트 폴더를 VM에 공유할 수 있다.** 스키마에 `Plan9`와 `VirtualSmb`가 있다. WSL은 Windows 드라이브를 Plan9(설정하면 virtio-fs)로 올린다. 다운로드나 사진 폴더를 안드로이드에 공유할 때 같은 방식을 쓸 수 있다.
8. **WSL의 새 컨테이너 VM은 binder 문제를 풀지 못한다.** `IWSLCVirtualMachineFactory`는 세션마다 HCS VM을 더 만들지만 언제나 WSL 커널로 부팅한다. 이미지를 바꾸는 `RootVhdOverride`는 "디버깅 전용"이다.

### 설계에서 바뀌는 것

| 영역 | WSL 컨테이너 | 전용 VM |
|---|---|---|
| 커널 | WSL 커널, binder 없음 | 자체 커널: WSL 커널 태그에 안드로이드 설정, binder 내장 |
| 그 위에서 도는 것 | WSL VM, 배포판, LXC, 안드로이드 | 안드로이드 init이 바로 |
| 사용자 WSL에 주는 영향 | 없음, 또는 커널을 바꾸면 모든 배포판 | 없음 |
| 격리 | 사용자 배포판과 커널 공유 | 자체 커널 |
| VM 생성 | WSL 서비스 | SYSTEM으로 도는 DSA 서비스 |
| 메모리 반환 | WSL 설정 | 같은 HCS 설정을 우리가 줌 |
| WSL도 같이 쓸 때 메모리 | VM 하나 | VM 둘: 커널 하나와 그 기본 메모리만큼 더, 측정 필요 |
| Hyper-V 소켓 VM ID | 알아내야 함 | 우리가 만들어서 앎 |
| 그래픽, 전송, 프로토콜, 세션, 컴포지터, HAL, 에이전트 | | 바뀌지 않음 |

이 조사 뒤 소유자가 INTENT D1과 D2를 전용 VM과 자체 커널로 바꿨다.

### Windows에서 확인할 것

1. Windows 11 Home에서 Virtual Machine Platform만 켜고, SYSTEM 서비스가 최소 `LinuxKernelDirect` VM을 `HcsCreateComputeSystem`으로 만든다.
2. 메모리 반환: 위 메모리 설정과 게스트 page reporting을 켠 상태에서, 게스트가 메모리를 놓으면 `vmmem` 프로세스가 줄어든다. 숫자와 장비를 기록한다.
3. Hyper-V 소켓: `GuestCommunicationServices` 레지스트리 항목 없이, VM의 ID와 위 보안 설명자로 호스트가 VM에 연결하고 VM의 연결을 받는다.
4. sparse 데이터 디스크: 게스트에서 데이터를 지우고 trim하면 VHDX 파일이 줄어든다.
5. WSL과 동시 실행: 안드로이드 VM과 WSL VM이 함께 돌고, 두 번째 커널만큼 늘어난 메모리를 잰다.

## R3. 두 디스크(읽기 전용 시스템 디스크와 사용자 데이터 디스크) 구조와 AOSP init 부팅 장치 인식

질문: INTENT D1과 D14는 시스템 이미지를 읽기 전용 공유 디스크로 두고 업데이트 때 통째로 바꾸며, 사용자 데이터는 별도의 데이터 디스크에 분리하도록 규정한다. 그러나 단일 디스크에서 두 디스크로 분리했을 때 AOSP init이 보조 디스크 파티션(/dev/block/by-name/userdata, /dev/block/by-name/metadata)을 찾지 못하고 멈추는 원인은 무엇이며, 이를 어떻게 해결할 수 있는가.

답(2026-10-24, 상태: 가설, 실기기 부팅 확인 전): **AOSP first-stage init은 부팅 디바이스(boot_devices) 아래의 블록 장치에만 `/dev/block/by-name/` 심볼릭 링크를 생성한다.** Hyper-V의 가상 SCSI 컨트롤러(storvsc)는 VMBus 버스 위에 존재하는데, 리눅스 커널의 VMBus 드라이버는 루트 디바이스(`vmbus_root_device`)를 ACPI 디바이스(`/sys/bus/acpi`)로 등록한다. AOSP init의 `DeviceHandler::FindPlatformDevice`는 디바이스 경로의 부모를 거슬러 올라가며 `/sys/bus/platform` 또는 `/sys/bus/amba`에 속하는지 확인하므로, VMBus 하위 디바이스는 플랫폼 디바이스로 인식되지 못한다. `androidboot.boot_part_uuid`를 넘기면 해당 파티션 UUID를 가진 단일 디스크만 부팅 디바이스로 예외 취급되어 두 번째 디스크가 완전히 무시된다.

가설과 해결책: 게스트 커널의 `drivers/hv/vmbus_drv.c`에서 VMBus 루트 디바이스를 ACPI companion이 아닌 플랫폼 디바이스(`pdev->dev`)로 등록하고, 이름을 `vmbus`로 지정(device_rename)한다. 이후 커널 명령줄에서 `androidboot.boot_part_uuid`를 제거하고 `androidboot.boot_devices=vmbus`를 지정하면, VMBus SCSI 컨트롤러에 연결된 두 디스크(LUN 0: system.vhdx, LUN 1: data.vhdx) 모두 플랫폼 부팅 장치로 인식되어 `/dev/block/by-name/*` 링크가 양쪽 디스크에 모두 구성될 것으로 예상한다.

### 근거로 삼은 자료

| 자료 | 버전 또는 커밋 |
|---|---|
| WSL 커널 소스 (`microsoft/WSL2-Linux-Kernel`) | `linux-msft-wsl-6.18.54.1` (`drivers/hv/vmbus_drv.c`) |
| AOSP `system/core/init` (`devices.cpp`, `block_dev_initializer.cpp`) | Android 16 / 17 AOSP latest release |
| Linux 커널 장치 모델 (`drivers/base/core.c`, `drivers/acpi/acpi_platform.c`) | 6.18 |

### 검증 방법

1. **커널 드라이버 분석**: `drivers/hv/vmbus_drv.c`의 `vmbus_acpi_add`가 `vmbus_root_device`를 `&device->dev`(ACPI 버스 디바이스)로 설정하는 지점을 확인했다. Device Tree 경로(`vmbus_device_add`)에서는 이미 `vmbus_root_device = &pdev->dev`로 플랫폼 디바이스를 쓰고 있음을 대조했다.
2. **AOSP init 부팅 장치 탐색 로직 확인**: `system/core/init/devices.cpp`의 `FindPlatformDevice`가 디렉터리 경로의 `subsystem` 링크가 `/sys/bus/platform`과 일치하는지 검사하는 구조를 확인했다. ACPI 버스에 속하면 `FindPlatformDevice`가 실패하고, `boot_devices` 매칭을 건너뛰어 `/dev/block/by-name/` 심볼릭 링크 생성이 누락된다.
3. **두 디스크 레이아웃 분리 구현**:
   - `system.vhdx`: `misc`, `vbmeta_a`, `vbmeta_system_a`, `vbmeta_system_dlkm_a`, `vbmeta_vendor_dlkm_a`, `boot_a`, `init_boot_a`, `vendor_boot_a`, `super`를 배치하고 읽기 전용으로 마운트한다.
   - `data.vhdx`: `metadata`(64 MiB)와 `userdata`를 담은 빈 GPT 템플릿(동적 VHDX)을 생성한다.
   - 커널 명령줄: `androidboot.boot_devices=vmbus`와 `blkdevparts=sda:...`를 전달한다.
4. **호스트 디스크 교체 경로 단순화 및 단일 디스크 하위 호환**: `system.vhdx`가 있는 경우 두 디스크를 연결하고, 기존 단일 디스크(`disk.vhdx`) 이미지인 경우 단일 디스크로 동작하도록 분기 처리했다.

### 현재까지 검증된 결과 (CI 및 단위 테스트)

1. **커널 패치 및 설정 검증**: `dsa/kernel/patches/vmbus-platform-device.patch` 적용 및 패치 검증 테스트(`test_android_config.py`)가 통과했다.
2. **이미지 생성 도구 검증**: `make-image.py`의 듀얼 디스크 분리 생성과 명령줄 생성 로직이 단위 테스트(`test_make_image.py`)를 통과했다.
3. **호스트 VM 구성 검증**: 두 디스크 이미지가 주어졌을 때 HCS SCSI 0번(읽기 전용 system.vhdx), 1번(쓰기 가능 data.vhdx)으로 정상 구성됨을 단위 테스트(`host_config.rs`)로 확인했다.
4. **단일 디스크 하위 호환 검증**: 기존 `disk.vhdx` 단일 디스크 이미지 역시 호스트가 그대로 인식하고 부팅 구성함을 단위 테스트(`host_config.rs`)로 확인했다.
5. **실기기 부팅 검증 항목(예정)**: 실기기 첫 부팅 시 콘솔 로그에서 `/sys/devices/platform/vmbus` 경로 확인, `hv_balloon`/`hv_pci`/메모리 핫애드 프로브 정상 동작 확인, `/dev/block/by-name/metadata` 및 `userdata` 마운트 성공 여부는 실기기 부팅 후 후속 변경에서 확인 결과를 기록한다.

### 결정

- 게스트 커널에 `dsa/kernel/patches/vmbus-platform-device.patch`를 적용한다.
- `make-image.py`는 `system.vhdx`와 `data.vhdx` 두 개의 VHDX를 빌드하도록 개편한다.
- 커널 명령줄에서 `androidboot.boot_part_uuid`를 제거하고 `androidboot.boot_devices=vmbus`를 기본으로 전달한다.
- 호스트 VM 구성은 SCSI 0번에 `system.vhdx`(ReadOnly: true), SCSI 1번에 사용자 데이터 디스크(ReadOnly: false)를 연결하되, 기존 단일 디스크 이미지 호환성을 유지한다.
- 실기기 부팅 검증 결과를 확인한 뒤 가설 상태를 검증 완료로 전환한다.

## R4. 게스트 메모리가 호스트로 돌아오지 않는 원인 (NFR-1, NFR-2)

코드와 이미지 설정, WSL 소스를 대조해 원인을 찾았다. 하드웨어에서 측정하지 않았다. 아래 절감량은 모두 추정치다.

### 확인한 자료

- WSL `master`의 `src/windows/service/exe/WslCoreVm.cpp` 1520-1543행과 `hcs_schema.h`
- WSL 커널 `linux-msft-wsl-6.18.54.1`의 `mm/vmscan.c` (`memory.reclaim`의 `swappiness=` 인자, 7738행)
- AOSP `android16-release`의 `libprocessgroup/profiles/cgroups.json` (메모리 컨트롤러는 cgroup v2), `system/sepolicy`의 `genfs_contexts` (`proc_page_cluster`), `ActivityManagerService.killAllBackgroundProcesses` (`CACHED_APP_MIN_ADJ` 이상만 끝낸다)
- `config-wsl`: `INIT_ON_FREE`, `PAGE_POISONING`은 꺼져 있어 부팅 때 전체 메모리를 건드리는 경로는 없다. THP는 `madvise`다. `LRU_GEN`은 꺼져 있다.
- 사용 PC의 `dsa-wm.log`: 창을 닫은 뒤 작업 집합을 비우는 동작은 실제로 일어난다 (224 MiB에서 0 MiB).

### 찾은 원인

1. HCS 문서에 `BackingPageSize`, `FaultClusterSizeShift`, `DirectMapFaultClusterSizeShift`가 없었다. WSL은 빌드 조건을 확인하고 이 셋을 설정하며, 메모 그대로 "vmmem을 낮추기 위한" 설정이다. DSA는 보고 단위만 128 KiB로 낮췄다. #136의 vmmem 2444 MB와 #145의 게스트 사용량 1.4-1.7 GiB 사이의 차이(약 700-1000 MB, 측정 시점은 다르다)가 이 원인과 맞는다.
2. 게스트는 메모리가 남으므로 커널이 스스로 스왑하지 않는다. zram이 켜져 있어도 비어 있고, 쉬는 프로세스의 익명 메모리는 그대로 남는다. 유휴 회수는 `drop_caches`뿐이었고, 캐시가 적으면 아예 건너뛰었다.
3. 유휴 회수는 데몬 시작 60초 뒤에 한 번뿐이어서 부팅 도중에 쓰이고, 그 뒤에는 작업이 열리기 전까지 다시 돌지 않았다.
4. 창이 모두 최소화된 `Dozing`에서는 데몬도 창 관리자도 작업이 열려 있다고 보고 아무것도 회수하지 않았다.
5. 사용자가 닫은 앱은 캐시 프로세스로 남는다. lmkd는 메모리 압박이 있어야 끝내는데, 유휴 게스트에는 압박이 없다.
6. ART의 힙 여유가 4 GiB 폰 값이다.
7. `memory.reclaim`을 `apps`, `system` 하위 그룹에 썼다. 이 파일의 소유자는 libprocessgroup이 정하므로 vendor_init이 열 수 있는지 코드만으로 확실하지 않다. 루트 그룹은 항상 존재하고 모든 프로세스를 포함한다.

### 결정과 추정 절감량

| 변경 | 추정 절감 | 근거 |
| --- | --- | --- |
| 작은 백킹 페이지와 64 KiB 폴트 | 500-1000 MiB | 위 vmmem과 게스트 사용량의 차이 |
| 유휴 시 cold 페이지를 zram으로 | 300-600 MiB | 익명 700-1000 MiB, LZ4 압축률 약 3배로 가정 |
| 닫힌 앱 종료 | 앱 하나당 30-70 MiB | zram으로 옮긴 뒤 남는 몫과 커널 자원 |
| ART 힙 여유 축소 | 40-100 MiB | 프로세스 약 25개, 각 1-4 MiB |
| 최소화된 창 작업 집합 비우기 | 창 하나당 약 40 MiB, 창 관리자 100-200 MiB | 앱 창 53 MB, 로그의 224 MiB |
| SecureElement 제거 | 약 15 MiB | 상주 프로세스 하나 |

`ro.config.low_ram`은 켜지 않는다. 멀티 윈도우가 꺼지는데, 모든 작업이 자유형 창이기 때문이다.

### 대가

- 작은 백킹 페이지는 게스트의 TLB 비용을 늘릴 수 있다. WSL과 같은 선택이다.
- 앱을 닫고 나면 다음 실행은 콜드 스타트다. `Dozing`에서 깨어날 때 zram 해제 시간이 더해진다. NFR-4에 영향이 있는지 측정해야 한다.
- 힙 여유가 줄어 할당이 많은 앱의 GC가 잦아진다.

### 코드만으로 확인하지 못한 것

- 위 절감량 전부. 특히 원인 1의 실제 크기. HCS의 기본 폴트 단위는 공개 문서에 없다.
- 실제 이미지의 `cgroups.json`과 `/sys/fs/cgroup/memory.reclaim`이 존재하는지. Android 17 빌드는 대상 파일을 내려받지 않아 보지 못했다.
- vendor_init이 그 파일을 쓸 수 있는지. 실패하면 init 로그에 남고 조용히 넘어간다.
- 재현 시 게스트 측 그래픽 버퍼: vkms dumb 버퍼는 shmem이고 매핑된 동안 스왑되지 않는다. `dsa memory`의 공유 메모리 줄로 크기를 본다.

## R5. 창 크기를 끄는 동안 프레임을 기다려도 되는가 (FR-4)

**기준과 방법.** 소유자 PC의 앱 창 로그(`%LOCALAPPDATA%\DSA\logs\dsa-app-window.log`, 2026-10-26부터 2026-10-29까지, #146 이전 빌드)에서 `stage=app.summary`의 `waited=` 값을 모아 보았다. 태스크 창이 크기를 받은 때부터 그 크기의 첫 프레임이 보일 때까지의 시간이다. 이 줄은 태스크마다 500 ms에 한 번만 남고, 그때는 창이 프레임을 기다리지 않고 앞서 나갔으므로, 드래그의 일부만 담은 표본이다. Intel Arc, Android 16 게스트, Clock과 Settings에서 나온 값이다.

**결과.**

| 표본 97개 | 값 |
|---|---|
| 중앙값 | 112 ms |
| 90번째 백분위 | 305 ms |
| 60 ms 안에 도착 | 11개 (11%) |
| 100 ms 안에 도착 | 40개 (41%) |

가장 빨랐던 구간의 창 요약은 중앙값 54 ms, 95번째 백분위 134 ms에서 180 ms다. 안드로이드가 프레임 간격 몇 개 안에 다시 그리는 경우에도 예산 60 ms와 거의 같고, 조금만 느려도 넘긴다.

**코드에서 확인한 것.**

- #146의 고정 예산 60 ms와 시간 초과 3회 포기로는 대부분의 드래그가 곧 늘이기로 떨어진다. 포기를 되돌리는 경로가 없어서(`should_wait`는 `timeouts < 3`일 뿐이고, 기다리지 않는 단계는 `step_ended`를 부르지 않는다) 그 드래그가 끝날 때까지 돌아오지 않는다.
- 게스트가 예산보다 조금 느리면 프레임은 도착하는데도 매 단계 시간 초과가 난다. 늦게 온 프레임이 도착할 즈음이면 창은 이미 다음 크기로 갔기 때문이다.
- #146은 `WM_SIZE` 안에서 기다린다. 그때는 창 사각형이 이미 바뀌었다. DWM이 그 메시지가 돌아올 때까지 새 사각형을 보여 주지 않아야 이 대기가 말이 되는데, 코드만으로는 확인할 수 없다. 보여 준다면 왼쪽과 위쪽 가장자리를 끄는 동안 그림이 밀리고 오른쪽과 아래쪽에 빈 띠가 남는다.

**결정.** 크기를 받기 전에 기다린다(`WM_WINDOWPOSCHANGING`). DWM이 어떻게 하든, 기다리는 동안 화면은 옛 창과 옛 프레임으로 온전하다. 새 크기의 프레임은 백 버퍼에 복사만 해 두고, 창이 크기를 받는 `WM_SIZE`에서 Present와 visual 커밋을 함께 한다. 예산은 태스크가 실제로 답한 시간의 1.5배를 50 ms에서 100 ms 사이로 맞춘다. 포기한 드래그는 250 ms 뒤부터 한 단계씩 다시 기다려 본다(실패할 때마다 두 배, 최대 2초). 그사이 예산 안에 온 프레임이 있으면 바로 기다리기로 돌아간다.

**실기에서 확인할 것.** 이 표본은 #146 이전 빌드의 값이다. 한 번에 크기 하나만 요청하는 지금의 드래그에서 답이 얼마나 빨라지는지, `budget=`이 어디에 자리 잡는지는 측정이 필요하다.

## R8. 앱 하나를 연 DSA의 메모리 내역과 줄일 곳 (NFR-1, NFR-2)

**기준.** 소유자 PC, develop dcbb771, Android `Active`, Settings 창 하나, `dsa memory`. 아래 절감량은 코드와 설정을 읽어 낸 추정치이고, 이 조사에서 빌드하거나 실행하지 않았다.

| 항목 | 측정값 |
|---|---|
| vm | 상주 670 MiB, private 1189 MiB |
| 창 관리자(dsa-wm) | 상주 119 MiB, private 501 MiB |
| 앱 창 | 38 MiB |
| 게스트 | 2908 MiB 중 809 MiB 사용. 페이지 캐시 276, 앱 익명 168, 커널과 나머지 336, zram 220 MiB를 62 MiB에 보관 |
| PSS 상위 | system_server 131, Settings 101, 에이전트 46, ext.services 28, media provider 23, permissioncontroller 18, networkstack 12, gceservice 12, devicelockcontroller 9, phone 9 |

**찾은 것과 바꾼 것.**

1. dsa-wm: 시작할 때 H.264, HEVC, VP9, AV1 디코더를 하나씩 열어 보았다(#181). 디코더마다 비디오 지원 D3D11 장치를 만들고 GPU의 미디어 드라이버를 프로세스에 올리며, 닫은 뒤에도 드라이버 DLL과 그 힙은 남는다. 이제 게스트가 처음 물을 때 자식 프로세스(`dsa-wm --probe-video`)가 확인하고 끝나며, 디코더는 게스트가 코덱을 열 때만 만든다. 추정 20-60 MiB(드라이버에 따라 다르다). 창이 하나도 보이지 않을 때는 작업 집합을 비우는 것에 더해 D3D11 장치를 `Trim`하고, 그 전후의 작업 집합, private, GPU 메모리(로컬, 비로컬)를 로그에 남긴다. 렌더러(gfxstream)는 게스트의 표면을 갖고 있어 내리지 않는다. 내리면 창을 다시 열 때 게스트 전체를 다시 그려야 한다.
2. 커널과 나머지 336 MiB: 이제 `dsa memory`가 `/proc/meminfo`의 SUnreclaim, KernelStack, PageTables, VmallocUsed, Percpu, CmaTotal, 휴즈 페이지, zram 저장분과 그 밖(드라이버, GPU와 DMA 버퍼)으로 나누고, 온라인 메모리 블록과 MemTotal의 차로 부팅 때 커널이 잡은 몫(코드, struct page, swiotlb, pstore)을 보인다. 2908 MiB는 3072 MiB VM에서 약 164 MiB가 빠진 값이다. struct page가 약 48 MiB(1.56%), ramoops는 1 MiB다. swiotlb는 Hyper-V의 VMBus 장치가 쓰지 않지만 끄는 것은 이 숫자를 본 뒤에 정한다.
3. 게스트 프로세스: Cuttlefish의 gceservice(`CuttlefishService`)는 이미지에서 뺀다. Cuttlefish 런처에 상태를 알리는 앱이고 DSA에는 그 런처가 없다. 기기 잠금(devicelock)은 기능을 내려 system_server가 서비스를 띄우지 않게 하고, 에이전트가 시작할 때 두 패키지가 남아 있으면 끈다. phone(TeleService)은 system_server가 부팅 때 바인딩하므로 남긴다. 캐시 프로세스 상한은 32에서 8로 낮춘다(`activity_manager_constants`). 추정 25-40 MiB.
4. 닫은 앱: 한 패키지의 마지막 창이 닫히면 에이전트가 그 앱을 끝내고(위젯이 놓여 있거나 포그라운드 서비스가 있으면 남긴다), 제어 데몬이 2초 뒤 캐시를 버리고 압축해 빈 페이지 보고로 호스트에 돌려준다. 앱마다 30-100 MiB가 몇 초 안에 돌아온다고 본다. 에이전트는 `closed <package>: returned N MiB in T ms`를 남긴다.
5. VM 크기: 호스트 메모리에서 Windows 몫(10%, 최소 2 GiB)을 뺀 값으로 키우는 경로와 `dsa settings set memory-limit`는 들어갔지만, 기본값(`auto`)은 소유자가 아래 대가를 결정할 때까지 지금의 3 GiB로 둔다(`SIZE_TO_COMPUTER`). 대가가 있다. 커널은 모든 메모리의 struct page를 부팅 때 만들고 채우므로, 최대 크기의 약 1.56%가 늘 상주한다. 32 GiB PC(VM 29.5 GiB)에서는 약 460 MiB로, 3 GiB일 때의 48 MiB보다 400 MiB 이상 많다. 이것은 NFR-2의 "대기 메모리가 눈에 띄게 늘지 않는다"와 충돌할 수 있으므로 실측해야 한다. 피하는 방법은 메모리를 작게 시작해 필요할 때 늘리는 것(Hyper-V의 메모리 핫애드, `hv_balloon`)인데, HCS로 만든 VM에서 쓸 수 있는지는 확인하지 못했다. 실측에서 늘어나면 사용자가 `dsa settings set memory-limit 8G`처럼 상한을 둘 수 있다.

**WSL과의 비교 (공개 자료, 측정 아님).** WSL2는 기본으로 호스트 메모리의 50%를 VM 최대 크기로 두고, 같은 struct page 비용을 낸다. 배포판 하나가 쉬고 있을 때 `vmmemWSL`은 공개된 사용자 보고에서 수백 MiB 수준이다. WSL은 `autoMemoryReclaim`(gradual은 cgroup 회수, dropcache는 캐시 버리기)과 빈 페이지 보고로 쓰지 않는 메모리를 돌려준다. DSA도 같은 장치(지연 커밋, 빈 페이지 보고, 차가운 페이지 힌트, 작은 백킹 페이지)를 쓴다. 차이는 안드로이드가 쉬는 동안에도 system_server와 앱 프로세스를 띄워 두는 데 있다. 그래서 목표는 프로세스 수를 줄이고 닫은 앱을 바로 끝내는 쪽이다.

**목표 (실측 전).** Settings 하나를 연 상태의 DSA 호스트 private 합 단기 400 MiB, 최종 250 MiB. 이번 변경 뒤 기대치는 dsa-wm private -20~-60 MiB, 게스트 프로세스 -25~-40 MiB, 닫은 앱마다 -30~-100 MiB(몇 초 안에)다.

**PC에서 잴 것.** `measure.ps1 -Part open`으로 Settings 하나를 연 상태의 각 프로세스 private와 `dsa memory`의 커널 내역을 기록한다. `-Part close`로 앱을 닫은 뒤 vm 상주 메모리가 몇 초 안에 얼마나 줄었는지 잰다. 32 GiB PC에서 `dsa settings set memory-limit 3G`와 `auto`를 바꿔 가며 `Dozing` 상주 메모리와 부팅 시간을 비교한다. `dsa-wm.log`에서 "no task window is shown" 줄의 GPU 메모리를 확인한다.

## R12. 실행 중 메모리를 늘리고 줄일 수 있는가 (NFR-1, NFR-2)

**질문.** VM을 처음부터 호스트 크기로 만들면 커널의 struct page가 최대 크기의 약 1.56%를 늘 차지한다(32 GiB PC에서 약 460 MiB). 작게 시작해 필요할 때 메모리를 더하고, 필요 없어지면 돌려줄 수 있는가.

**확인한 것 (코드와 공개 자료, 실행하지 않음).**

- DSA 커널(`config-wsl` 6.18.54.1 + `android.config`): `CONFIG_MEMORY_HOTPLUG=y`, `CONFIG_MEMORY_HOTREMOVE=y`, `CONFIG_MHP_DEFAULT_ONLINE_TYPE_ONLINE_AUTO=y`(더한 메모리를 커널이 바로 온라인으로 올린다), `CONFIG_MHP_MEMMAP_ON_MEMORY=y`, `CONFIG_HYPERV_BALLOON=y`, `CONFIG_PAGE_REPORTING=y`, `CONFIG_VIRTIO_MEM=m`. Hyper-V의 balloon 드라이버(`hv_balloon`)는 Dynamic Memory 프로토콜로 호스트의 핫애드 요청을 받아 메모리를 더하고(`hot_add` 매개변수, 기본 켜짐), 풍선을 부풀려 메모리를 돌려준다. virtio-mem은 모듈이고 Hyper-V에는 그 장치가 없다.
- HCS: microsoft/hcsshim의 `internal/uvm` `UpdateMemory`는 실행 중인 유틸리티 VM에 `ResourcePath: VirtualMachine/ComputeTopology/Memory/SizeInMB`, `RequestType: Update`, `Settings: <MiB>`인 modify 요청을 보내 메모리 크기를 바꾼다. 같은 요청을 `HcsModifyComputeSystem`으로 보낸다.
- WSL2: VM 최대 크기를 처음부터 정하고(기본 호스트의 50%), 빈 페이지 보고와 `autoMemoryReclaim`으로 돌려준다. 실행 중 핫애드는 쓰지 않는다. 같은 struct page 비용을 낸다.

**설계.** VM은 3 GiB로 시작한다. 제어 데몬이 2초마다 `/proc/meminfo`와 `/proc/pressure/memory`를 읽고, 부족하면 바로, 아니면 30초마다 `GuestMemoryPressure`를 보낸다. 호스트는 가용 메모리가 전체의 1/5(최소 512 MiB) 아래이거나 메모리 대기가 10% 이상이면 현재 크기의 절반(최소 1 GiB)을 더한다(5초 간격, 최대는 호스트 메모리에서 max(10%, 2 GiB)를 뺀 값 또는 사용자 상한). 30초 동안 여유가 있으면 쓰는 양의 1.5배(최소 1 GiB 여유)로 줄이고 시작 크기 아래로는 내리지 않는다. 앱을 닫으면 #210의 즉시 반환(앱 종료, 캐시 버림, 빈 페이지 보고)이 함께 동작한다.

**실기에서 확인할 것.** 이 경로는 실행해 보지 않았다.

1. 그 modify 요청을 이 VM(HCS 스키마 2.x, Linux 커널을 직접 부팅하는 VM)이 받는지. 거절하면 서비스 로그에 `memory: Android's memory could not be set to N MiB`가 남는다.
2. 받아들인 뒤 게스트 `MemTotal`이 실제로 늘어나는지. 30초 안에 늘지 않으면 `memory: Android's memory no longer grows`가 남고, VM은 시작 크기(3 GiB)를 유지한다. 이때 더 큰 고정 크기로 대신 시작하지 않는다. 그 결정은 소유자에게 묻는다.
3. 크기를 줄이는 요청에 `hv_balloon`이 풍선을 부풀리는지, `vmmem` 상주 메모리가 줄어드는지.
4. 측정: `measure.ps1 -Part grow -Package <무거운 앱들>`.

## R13. 크기를 끄는 동안 설정 앱이 늘어나 보이는 문제 (FR-4)

**기준.** 소유자 PC, develop dcbb771, 200% 배율, 설정 앱 창의 가장자리를 끌어 크기를 바꾼다. 소유자가 본 것: 예전보다 훨씬 낫지만, 설정 앱은 끄는 동안 내용이 늘어나 보인다. 아래는 코드를 읽고 낸 원인과 바꾼 것이며, 이 조사에서 빌드하거나 실행하지 않았다.

**원인.**

1. 표시 규칙. 새 크기의 프레임이 오기 전까지 창은 마지막 프레임을 창 크기로 늘렸다(`visual_scale`이 가로와 세로를 따로 맞췄다). R5의 대기 예산(50-100 ms)을 넘기면 창은 크기를 받고 늘린 프레임을 보인다. 설정 앱은 다시 배치하는 데 100 ms 안팎이 걸리므로(R5 표본의 중앙값 112 ms), 드래그의 대부분 단계가 예산을 넘기고 늘린 그림이 보인다. 가로와 세로의 비율이 다르게 늘어나 글자가 찌그러진다. Windows의 일반 앱은 크기를 바꾸는 메시지 안에서 다시 배치하므로 창과 내용이 늘 같은 크기다.
2. 크기 전달 속도. 드래그 중의 크기는 호스트(`coalesce`)에서 모아 마지막 것만 보내고, 에이전트(`TaskArranger`)는 앞 크기가 창 관리자에 반영(`bounds`가 바뀜)되면 바로 다음 크기를 준다. 반영은 프레임 하나 안쪽(16 ms 확인 주기)이지만, 앱이 그 크기로 그리는 데는 100 ms가 걸린다. 그래서 앱은 한 크기를 다 그리기 전에 새 크기를 계속 받는다. 크기마다 레이아웃이 새로 시작되고, 창 관리자는 크기가 바뀐 창의 다음 버퍼를 기다리는 동기화(BLAST sync)를 다시 건다. 사용자가 기다리는 프레임이 계속 뒤로 밀린다. 리사이즈 추적으로 확인할 수 있다. 이 원인이 맞다면 `stage=service.forwarded` 줄은 16-30 ms 간격으로 촘촘한데, 그 크기의 `stage=wm.frame`은 대부분 오지 않고 다음 크기로 넘어간다. 이 조사에서는 추적 로그를 받지 못해 코드로만 판단했다.
3. 이미 맞는 것. 드래그 중에는 화면 크기(dp)를 고정해 액티비티를 다시 만들지 않고(`resizeTaskLive`), 버퍼는 단계 크기로 잡아 대부분의 크기 변경에서 다시 만들지 않는다(#151). 이 둘은 그대로 둔다.

**처음 고친 것과 소유자의 판단.** 처음에는 마지막 프레임을 늘리지 않고 1:1로 두고, 창이 커져 드러난 곳은 프레임 가장자리 색으로 채우고, 작아지면 잘랐다. 소유자는 이것도 받아들이지 않았다. 늘리기, 채우기, 자르기 모두 창 크기와 그 안의 프레임 크기가 다른 순간을 만든다. 기준은 Windows의 일반 앱처럼 어느 순간에도 창 크기가 그 안에 보이는 프레임 크기와 같은 것이다.

**바꾼 것.**

1. 창은 그 크기의 프레임과 함께만 크기를 바꾼다(`FrameFollow`). 드래그 중 `WM_WINDOWPOSCHANGING`에서 제안된 크기는 게스트에 요청하고, 창의 실제 크기는 지금 보이는 프레임의 크기로 되돌린다(끌지 않는 쪽 가장자리는 제자리에 둔다). 다른 크기의 프레임이 오면 백 버퍼에 복사만 하고 창에 알린다(`WM_APP_FRAME_READY`). 창은 `SetWindowPos`로 그 프레임의 크기를 받고, `WM_SIZE`에서 그 프레임을 Present하고 visual을 커밋한다. 새 창 크기와 그 크기의 프레임이 한 커밋으로 화면에 닿는다. 창이 크기를 고른 뒤 받기 전에 온 새 프레임은 기다린다(최대 100 ms). 그래서 받는 크기의 프레임이 백 버퍼에서 지워지지 않는다. 프레임이 오지 않는 동안 창은 이전 크기에 머문다. 마우스는 앱이 그리는 속도로 따라간다.
2. #146과 #151의 대기 예산(`ResizeSync`)과 늘리기는 없앴다. 창이 프레임을 기다리며 메시지를 붙잡지 않으므로 드래그가 멈칫하지 않는다.
3. 드래그의 크기를 앱이 그리는 속도에 맞춘다(`ResizePacer`). 드래그 중의 크기를 하나 보낸 뒤, 앱이 그 크기의 프레임을 그릴 때까지(dsa-wm이 그 크기의 프레임을 받을 때까지) 다음 크기를 붙잡고, 풀리면 그동안 쌓인 것 중 마지막 크기만 보낸다. 앱이 그 크기를 끝내 그리지 않아도(게스트의 최소 크기 등) 150 ms 뒤에는 보낸다. 드래그를 끝내는 크기(live 아님)는 붙잡지 않으므로, 드래그가 끝난 크기에 정확히 닿는다. 붙잡힌 크기는 추적에 `stage=wm.request_held`로 한 번 남는다.
4. 입력과 접근성 좌표는 1:1이다. 창과 프레임의 크기가 늘 같기 때문이다.

**전후 비교 (추론, 실측 아님).** 전: 설정 앱은 100 ms마다 그릴 수 있는데 크기는 16-30 ms마다 바뀌었다. 앱이 그린 프레임은 이미 지나간 크기였고, 창은 늘린 프레임을 보였다. 후: 앱은 자기가 다 그린 직후에 가장 최근의 크기를 받는다. 창은 대략 100-150 ms마다 한 걸음씩 마우스를 따라가고, 그 사이에는 이전 크기와 이전 프레임 그대로다. 찌그러지거나 잘리거나 채운 그림은 없다. 대가는 느린 앱에서 창 가장자리가 마우스보다 뒤처져 보이는 것이다.

**참고 구현과의 비교.** Windows 창 작업의 참고였던 Kotlin/Native 창 구현(`thisisthepy/compose-multiplatform-core-extended`, `extended/window/windows-shared/c/win32_window.c`와 `win32_resize_sync.h`, `feat/win32-resize-sync` 브랜치, 2026-11-05에 읽음)을 코드로만 비교했다. 실행하지는 않았다.

- 기본 경로: 프레임을 그 크기를 바꾼 `SetWindowPos` 안의 `WM_SIZE`에서 그리고 Present한 뒤 `DwmFlush`로 DWM이 합성할 때까지 기다린다. 기다리지 않으면 단계가 DWM보다 빨리 쌓여, 새 사각형에 옛 크기의 그림이 보일 수 있다고 적혀 있다. DSA도 `WM_SIZE` 안에서 Present와 커밋을 하지만 `DwmFlush`는 없었다. 이것을 들였다. 프레임을 따라 크기를 받은 단계마다 `WaitForCommitCompletion` 뒤에 `DwmFlush`를 부른다.
- 더 강한 경로(선택 사항, `DXC_RESIZE_SYNC=1`, Windows 11): Windows 11 설정 앱과 XAML이 쓰는 비공개 크기 동기화다. user32 서수 2615(`EnableResizeLayoutSynchronization`)를 창과 크기 0인 자식 창에 켜고, `WM_WINDOWPOSCHANGED`에서 서수 2614로 동기화 토큰을 받는다. 그림은 스왑체인이 아니라 Windows.UI.Composition 그리기 표면에 그리고 `IDCompositionDesktopDevicePartner6::SynchronizedCommit(token)`으로 커밋한다. 그러면 DWM이 창 테두리를 그 커밋까지 붙잡는다. 참고 구현의 기록에 따르면 스왑체인의 Present는 토큰을 실은 커밋에 속하지 않아 구멍과 낡은 프레임이 보였다. DSA는 프레임이 올 때까지 창 크기 자체를 바꾸지 않으므로 이 차이가 작다. 남는 차이는 크기를 받는 한 단계 안에서 테두리와 스왑체인 Present가 다른 DWM 프레임에 닿을 수 있다는 점이다. 이 경로는 들이지 않았다. 비공개 서수와 vtable 슬롯에 기대고, 참고 구현에서도 선택 사항이며, 프레임 경로를 스왑체인에서 그리기 표면으로 바꿔야 하기 때문이다. PC 녹화에서 크기를 받는 단계에 어긋난 프레임이 보이면 다음 후보다.

**PC에서 잴 것.** 200% 배율에서 `dsa settings set resize-trace on`을 켜고 설정 앱 창의 오른쪽 아래 모서리와 왼쪽 가장자리를 각각 3초씩 끈다. `dsa-app-window.log`의 `stage=app.summary`에서 `waited=` 중앙값과 90번째 백분위를 R5의 표(112 ms, 305 ms)와 비교하고, `stage=app.frame_kept` 수를 본다. `dsa-wm.log`에서 `stage=wm.request_held` 수와 `stage=wm.request`의 간격을 본다. 끄는 동안 녹화해, 창 크기와 내용이 어긋난 프레임이 없는지, 손을 뗀 크기에 창이 정확히 멈추는지 확인한다.

## 부록 A. Microsoft에 낼 이슈 초안

R1의 선택지 D다. [microsoft/WSL](https://github.com/microsoft/WSL/issues)에 내는 이슈로, 아직 내지 않았다. 소유자가 낸다. 이슈는 영어로 내므로 본문은 영어로 둔다.

내기 전에 할 일:

- 현재 WSL 태그에서 크기 변화를 잰다(`size vmlinux`, binder 있을 때와 없을 때).
- 지금 binder와 binderfs를 켜는 데스크톱 배포판을 확인해 설정 파일을 인용하거나, 그 문장을 뺀다.
- 분석에 대한 링크를 공개된 위치로 바꾼다. 이 문서는 `develop`에만 있다.

---

**Title:** Enable `CONFIG_ANDROID_BINDER_IPC` and `CONFIG_ANDROID_BINDERFS` in the WSL kernel

**Summary**

Please build the binder IPC driver and binderfs into the WSL kernel:

```
CONFIG_ANDROID_BINDER_IPC=y
CONFIG_ANDROID_BINDERFS=y
CONFIG_ANDROID_BINDER_DEVICES=""
```

With an empty `ANDROID_BINDER_DEVICES`, the kernel creates no global binder device nodes.
Binder is reachable only through a binderfs instance that someone mounts, so a WSL
distribution that does not use it sees no change.

**Why**

Running Android userspace in a container (the approach of Waydroid on Linux desktops)
needs binder. On WSL today the only way to get it is to replace the kernel with `kernel=` in
`.wslconfig`. That affects every WSL 2 distribution the user has, stops the kernel updates
WSL ships, and is refused on machines whose policy disallows custom kernels.

A module is not a way out. Upstream declares `ANDROID_BINDER_IPC`, `ANDROID_BINDERFS` and
`ANDROID_BINDER_IPC_RUST` as `bool`, and `kernelModules` without `kernel` is refused by
design. Built out of tree against `linux-msft-wsl-6.18.54.1`, the driver imports 14 symbols
that the kernel does not export (among them the `security_binder_*` LSM hooks,
`task_work_add`, `file_close_fd` and `zap_page_range_single`), so it cannot load. Working
around that means looking up private symbols at run time, which breaks with kernel updates
and runs into IBT, which the WSL kernel enables.

**Cost and risk**

- Size: the binder driver, its allocator and binderfs, built in. To be measured on the
  current tag before submitting (`size vmlinux` with and without).
- Attack surface: binderfs sets `FS_USERNS_MOUNT`, so an unprivileged user namespace can
  mount an instance. This is the main point to discuss. Binder is the same code that runs on
  every Android device and is heavily fuzzed there, and it is already enabled in the kernels
  of mainstream desktop Linux distributions that support Waydroid (to be confirmed with
  current config files before submitting).
- Maintenance: none beyond following upstream; the driver is in mainline.

**What this enables**

Android userspace in a WSL container, sharing WSL's VM and memory management instead of
starting a second VM, for any project that wants it.

## 터치 지연의 구간별 원인 (FR-5)

**확인 대상**: develop 87e802d, gfxstream d047a572, mesa3d 224e91e3, 커널 `linux-msft-wsl-6.18.54.1`.

**방법**: 터치 메시지부터 화면까지 코드를 구간별로 읽었다. 이 조사에서는 빌드도 실행도 하지 않았으므로 아래에 시간을 적은 수치는 측정값이 아니다. 실측은 아래 절차로 한다.

**찾은 원인**

- 게스트 Vulkan 드라이버가 제출마다 앱의 host-visible 메모리 전부를 hvsocket으로 보냈다(패치 0003의 `flushShadowedMemory`, `VK_WHOLE_SIZE`). 앱이 쓰는 메모리에 비례해 프레임마다 전송량이 늘어나므로, 가장 유력한 원인이다. 이제 4 KiB 블록의 해시를 비교해 바뀐 블록만 보낸다. 해시는 블록마다 8바이트를 쓴다.
- 에이전트는 포인터 주입을 작업 조회나 앱 실행 같은 binder 호출과 같은 스레드에서 처리했다. 이제 전용 스레드에서 처리하고, 밀린 이동 이벤트는 최신 것으로 합친다.
- 게스트 커널이 `PREEMPT_NONE`이었다(`config-wsl`). 실시간 스레드도 커널 코드가 스스로 양보할 때까지 기다린다. 이제 `CONFIG_PREEMPT=y`이다.
- 모든 로그(`-b all *:V`)가 에뮬레이션 직렬 포트로 나갔다. 바이트마다 VM 밖으로 나가므로 프로세서 시간을 빼앗는다. 이제 `main,system,crash` 버퍼의 정보 수준 이상만 보낸다.
- `dsa-channeld`의 게스트에서 호스트 방향 프레임은 수신 대기(20 ms)가 끝나야 나갔다. 이제 방향마다 스레드를 둔다. 터치 이벤트 자체는 이 경로가 아니고, 새 창의 `TaskUpdated`와 `TaskFrames`가 이 경로다.
- 창이 없는 `dsa-wm`과 서비스는 Windows에서 백그라운드 작업으로 취급될 수 있고, 호스트 스레드에 우선순위 설정이 없었다. 이제 효율 스케줄링을 끄고 입력과 프레임 스레드의 우선순위를 올린다.
- 터치 창에 `TWF_WANTPALM`이 없어 손바닥 판별 지연이 생길 수 있었고, 길게 누르기 판별도 켜져 있었다. 둘 다 끈다.
- 스왑체인 버퍼가 2개라 DWM이 보여 주는 버퍼에 복사가 걸리면 다음 수직 동기까지 기다릴 수 있었다. 이제 3개다.

**원인이 아니라고 본 것**: 입력 스트림(`stream_input`, `Session::send_direct`)은 폴링과 응답 대기가 없다. hvsocket과 파이프의 대기는 이벤트로 깨어난다. 주입은 비동기 모드다. 표시 복사는 표시마다 최신 프레임만 남긴다.

**코드로 확인하지 못한 것**: 구간별 실제 시간, 앱별 host-visible 메모리 크기, Hyper-V 직렬 포트의 바이트당 비용, `TWF_WANTPALM`이 `WM_POINTER`에 미치는 효과, 유휴 뒤 회수된 페이지를 다시 읽는 비용(`memory.reclaim`, zram).

**측정 절차**

1. 호스트 설정의 `kernel_cmdline`에 `androidboot.hardware.dsa_input_trace=1`을 더하고, `%LOCALAPPDATA%\DSA\input-trace` 파일을 만든 뒤 DSA를 다시 시작한다.
2. 앱 창을 20번 누른다.
3. `uv run scripts/input-trace.py`로 모아 호스트 시계 순으로 합친다. 호스트 줄은 `%LOCALAPPDATA%\DSA\logs\*.log`와 `C:\ProgramData\DSA\logs\*.log`에서, 게스트 줄은 `dsa logcat -d -b all`에서 읽는다. 안드로이드 로그는 이제 파일로 남지 않고 메모리에만 있으므로(FR-21), 누른 직후에 모은다.

구간은 `app.pointer`, `agent.inject`, `driver.flush`, `hwc.frame`, `wm.frame`, `app.present` 순이다. 호스트와 게스트의 시계 차이는 합친 간격에 포함된다.

**결정**: 위 원인을 고친다(PR 참조). 수치 예산은 실측 뒤에 정한다.

## 터치 지연의 구간별 실측 (FR-5)

**확인 대상**: develop 7a20614, Windows 11, Intel Arc, Settings 앱, 새 데이터 디스크, 추적 켬. 게스트 로그(02:09~02:26)와 호스트 로그(02:35까지), 외부 화면 캡처 20회. 빌드 입력은 `aosp_cf_x86_64_only_phone-target_files-15581820`이다.

**방법**: `input-trace`, Davey, 서비스 로그를 시간순으로 합쳤다. 게스트 `mono`, HWUI 타임스탬프는 같은 `CLOCK_MONOTONIC`이므로 게스트 안에서는 직접 비교한다. 호스트와 게스트의 UTC 차이는 버튼을 뗀 이벤트(호스트에서 에이전트까지 3~4 ms)와 프레임(게스트에서 호스트까지 1 ms)으로 보아 수 ms 이내다. systrace가 없으므로 앱 내부의 대기 원인은 아래에서 추론이라고 표시한다.

**구간별 측정값**

| 구간 | 값 | 근거 |
| --- | --- | --- |
| 누름에서 첫 표시 (호스트 안) | 최소 23.7, 중앙 43.8, p95 54, 최대 56 ms | 20회, `app.pointer`에서 `app.present`까지. 누르기 전 350 ms쯤 프레임이 없었으므로 누름에 대한 반응이다 |
| 누름에서 화면의 첫 변화 (외부) | 최소 95, 중앙 136, p95 165 ms, 캡처 지연 10~20 ms 포함 | 같은 20회 |
| 첫 표시 뒤 프레임 간격 (400 ms 안) | 중앙 16.7, p90 28.7, p99 53.5, 최대 231 ms. 33 ms 이상 7%, 50 ms 이상 1.3% | 378개 |
| hwcomposer compose | 중앙 5.1, p90 13, p99 28.6, 최대 47.8 ms | 338개 |
| hwcomposer post, `app.present` | post 0.1~1.2 ms, present 중앙 0.8, p95 4.3 ms | 호스트 복사는 대기의 원인이 아니다 |
| 호스트 왕복 `wm.frame` | 중앙 0.8, p95 4, p99 8.6, 최대 510 ms | 3146개. 큰 값은 첫 프레임과 창 크기 변경 중에만 나온다 |
| 호스트에서 주입까지 | 누름 2회 29, 40 ms. 뗌 2회 3, 4 ms | 표본이 2개뿐이고, 20회 묶음의 첫 표시 44 ms와 맞지 않는다. 그 묶음의 게스트 로그는 없다 |
| `inject` | 16.1 ms 1회, 나머지 0.4~1.0 ms | 16.1 ms는 3분 넘게 쉰 뒤의 첫 누름이다 |
| 드라이버 스캔 | 제출마다 4~5 MB, 1.4~3.3 ms | 전송은 제출마다 8~25 KB, 0.3~0.5 ms |

**찾은 것 (증거 순)**

1. **오디오 스택 전체가 SELinux 거부 하나로 죽어 있었다.** `hal_audio_default`와 `mediaswcodec`가 만든 memfd는 `tcontext=tmpfs`이고 플랫폼 정책은 그 파일에 `getattr`만 준다. libcutils의 시험 `ftruncate`가 `Permission denied`로 실패하면("no memfd support") 그 프로세스는 ashmem으로 돌아가는데, DSA 커널에는 그 장치가 없다. 그 결과 FMQ가 무효(`mmap failed: 9`, `command FMQ is invalid`)가 되고, `openOutputStream`이 실패하며, 오디오 정책이 시작하지 못한다. 여기서 두 가지 낭비가 나온다.
   - `system_server`가 `listAudioPorts`를 초당 10번 재시도한다. 02:15 이후 직렬 로그 바이트의 77%가 이 줄이다.
   - 클릭할 때마다 `AS.SfxHelper`가 효과음을 다시 디코드하다 실패한다. 뗀 직후 여러 스레드가 코덱을 만들고 없애는 일이 55~160 ms 이어진다(02:24:52.26~52.42, 02:24:54.63~54.69). 앱이 클릭에 답해야 하는 바로 그때다.
2. **UWB.** `android.hardware.uwb` 기능은 선언되어 있는데 HAL APEX는 제거되어 있다. `system_server`의 스레드 2개가 부팅 내내 HAL을 기다리고, `servicemanager`는 초당 2번쯤 HAL을 시작하려고 스레드를 만든다. 직렬 로그의 16%다.
3. **`seriallogging`.** 없는 콘솔에 쓰므로 바로 끝나고 init이 5초마다 다시 시작한다. 194번 끝났고, 끝날 때마다 `flags_health_check`도 돈다.

**원인이 아니라고 본 것**

- 프레임 게시가 호스트 복사를 기다리는 시간: post는 1 ms 안팎이다.
- 호스트 복사와 표시 쪽 Present: `Present(0, 0)`이고 `took`이 작다.
- acquire fence: fence는 -1이다. 이 측정 당시에는 호스트가 이미지 해제를 대기열에 넣자마자 답했으므로, 패치 0008의 설명과 달리 렌더링이 끝났다는 보장이 없었다. 패치 0010(#151)부터 GPU 작업이 끝난 뒤 답한다. 이 때문에 앱의 present가 늦어질 수 있어 다음 측정에서 확인한다.
- 오디오 재시도의 CPU 자체: 초당 10번의 binder 호출은 작다. 낭비의 주된 부분은 직렬 포트 바이트인데, 바이트당 비용은 아직 측정하지 않았다.
- `SIGABRT`로 죽는 `main`: 이 로그에는 없다. 시그널로 끝난 서비스도 없다.

**1초 멈춤 (Davey 2건)**

Davey는 이 로그에 2건뿐이고 둘 다 액티비티 전환이다. 리플만 있는 20회에서는 최대 간격이 231 ms다.

- `duration=1093ms`: `SubSettings` 시작(52.186)부터 첫 순회(53.19)까지 메인 스레드가 페이지를 만든다. `Displayed +1s218ms`. Settings의 dexopt는 `verify`이므로 프로파일 없는 새 디스크에서 인터프리터와 JIT로 도는 첫 실행이다. 이 로그에는 두 번째 실행이 없어 첫 실행 비용을 빼 낼 수 없다.
- `DequeueBufferDuration=835ms`: 뒤로 가기 뒤 54.627부터 55.859까지 `hwc.frame`, `wm.frame`이 하나도 없다. 그 앞 compose는 1~5 ms이므로 호스트가 버퍼를 잡은 것이 아니다. `SurfaceFlinger`가 새 버퍼를 올리지 않은 것이다. AOSP의 BLAST 동기화는 전환이 끝날 때까지 참가 창의 버퍼를 묶어 두므로, 돌아갈 액티비티가 그릴 때까지 앱의 버퍼 3개가 소진되는 것으로 추론한다. 같은 때 `PendingStartActivityUids delay:1044ms`도 나온다. 추론이므로 다음 측정의 `waited`와 systrace로 확인한다.

**결정**: 위 1~3을 고치고, 소리가 호스트로 가는 경로가 없으므로 클릭 소리를 끈다(PR 참조). 드라이버 스캔은 고치지 않는다. 제출마다 2~3 ms이고, 앱이 언제 쓰는지 알릴 수단이 없어 건너뛰면 화면이 깨진다. 실측에서 다음 항목을 확인한다.

- 호스트 첫 표시 44 ms와 외부 첫 변화 약 120 ms 사이의 약 75 ms. `app.present` 뒤 DWM은 많아야 두 프레임이다. 나머지는 첫 프레임들에 눈에 띄는 변화가 없어서로 보인다. 마우스를 누를 때의 호버 해제와, 스크롤 안의 뷰가 누름 표시를 `ViewConfiguration.getTapTimeout()`(100 ms)만큼 미루는 것이 후보다. 스크롤 없는 버튼과 목록 행의 첫 변화 시간을 비교하면 갈린다.
- 앱이 쓰는 시간이 재지 않은 채로 남아 있으므로 `debug.sf.late.app.duration`을 줄이는 것은 보류한다. 놓치는 프레임이 늘 수 있다.
- 작업이 열린 동안의 `MostlyCache` 회수(02:21:53)는 익명 메모리 1 GiB를 zram으로 보내고 캐시를 버린다. 3분 뒤 첫 누름의 `inject=16.1`과 첫 페이지의 비용에 얼마나 기여하는지 재지 않았다.

## 클릭 반응의 100 ms와 300 ms 공백 (FR-5)

**확인 대상**: AOSP android16-qpr1-release 소스(GitHub 미러의 `platform_frameworks_base`, `platform_packages_apps_settings`), ART Service(LineageOS `android_art` lineage-23.0, 같은 Android 16 기반), androidx RecyclerView(androidx-main). 실측은 소유자 PC의 Android 16 이미지 16.0.0-15581820-7, 새 디스크, Settings 앱이다.

**실측 (PC 세션)**

- 누름에서 게스트 첫 `hwc.frame`까지 p50 26 ms(20회). 입력 경로는 병목이 아니다.
- 길게 누름에서 화면의 첫 픽셀 변화까지 p50 101 ms, 대부분 98~105 ms(20회). 이전 빌드는 136 ms였다.
- 20 ms 클릭에서 첫 픽셀 변화까지 419 ms(1회). 게스트 프레임은 누른 뒤 33, 67 ms에 나오고 384 ms까지 없다. 33, 67 ms 프레임에는 눈에 보이는 변화가 없다. 그 클릭의 `SubSettings` `Displayed`는 +314ms다.
- Settings는 ArtService 로그에서 `CompilerFilter=verify`다.

**방법**: 소스를 읽었다. 이 조사에서는 빌드도 실행도 하지 않았으므로, 아래의 시간은 소스의 상수이거나 계산이다.

**찾은 것**

1. **목록 행의 누름 표시는 탭 타임아웃만큼 늦게 시작한다.** `View.onTouchEvent`는 `ACTION_DOWN`에서 `isInScrollingContainer()`이면 `setPressed`를 바로 부르지 않고 `postDelayed(mPendingCheckForTap, mTapTimeoutMillis)`로 미룬다(View.java 18396~18411). 조상 중 하나라도 `shouldDelayChildPressedState()`가 참이면 그렇다. `ViewGroup`의 기본값이 참이고 RecyclerView는 이를 바꾸지 않으므로, Settings의 모든 행이 이 경로를 탄다. 마우스 원본(`SOURCE_MOUSE`)도 예외가 없다. 마우스를 다르게 다루는 곳은 보조 버튼의 상황 메뉴뿐이다(8372). 값은 프레임워크 리소스 `config_tapTimeoutMillis`(100)이고, 프로세스마다 한 번 읽는다(ViewConfiguration.java 1605~1639). 시스템 속성이나 `Settings.Secure`로는 바꿀 수 없다.
2. **Android 16의 리플은 처음 80 ms쯤 거의 보이지 않는다.** `RippleDrawable`은 `FORCE_PATTERNED_STYLE = true`(161)라 패턴 리플을 쓴다. 진입 애니메이션은 450 ms 동안 진행도를 0에서 0.5로 올리고(`RippleAnimationSession` 42, 175~186), 셰이더는 진행도 0.13까지 알파를 0에서 1로 올리며 반지름은 0에서 시작한다(RippleShader.java 102~122). 보간 곡선(0.4, 0, 0.2, 1)으로 계산하면, `setPressed` 뒤 33 ms에 알파 5%와 반지름 0.7%, 67 ms에 25%와 3.2%, 100 ms에 67%와 8.8%다. 짧은 클릭에서도 진입 450 ms는 줄지 않는다(`computeDelay`, 136~139). 이 상수들은 컴파일 시점 값이라 이미지로 바꿀 수 없다.
3. **누르기 직전의 호버 해제가 다시 그리기를 부른다.** `ACTION_HOVER_EXIT`는 `setHovered(false)`와 `refreshDrawableState()`를 거쳐, 리플 배경의 호버 표시(불투명도 0.2)를 80 ms 동안 선형으로 지운다(RippleDrawable 150, 832~884). 26 ms의 첫 프레임은 이것일 가능성이 높다(추론).
4. **클릭은 누름 표시를 기다리지 않는다.** `ACTION_UP`에서 `PerformClick`을 바로 `post`하고, 누름 해제만 64 ms 뒤로 미룬다(View.java 18333~18366). Settings의 `onPreferenceStartFragment`에서 `SubSettingLauncher.launch()`를 거쳐 `startActivity`까지, 지연을 넣는 코드는 없다. 클릭 뒤 300 ms 공백은 액티비티 시작과 첫 그리기이며, Settings 쪽이 일부러 기다리는 것이 아니다.
5. **ART는 첫 부팅에 앱을 컴파일하지 않는다.** 첫 부팅의 dexopt(`ArtManagerLocal.onBoot`)는 모든 패키지를 `pm.dexopt.first-boot`로 처리한다(`getDefaultPackages`, `REASON_FIRST_BOOT`). AOSP 기본값은 `verify`라서 검사만 하고, 앱은 인터프리터와 JIT로 돈다. 사용 중 모인 프로파일로 컴파일하는 백그라운드 dexopt 작업은 기기가 유휴이면서 충전 중이고 배터리가 부족하지 않을 때만, 하루 간격으로 돈다(BackgroundDexoptJob.java 135~143). 에이전트는 배터리가 없는 호스트에서 상태를 `UNKNOWN`으로 둔다. 그러면 JobScheduler가 충전 중으로 보지 않아 이 작업이 아예 돌지 않을 수 있다(추론, 확인하지 않음). `cmd package compile`은 root나 shell만 부를 수 있어서(`ArtShellCommand.enforceRootOrShell`) 에이전트가 대신할 수 없다.

**측정과 맞지 않는 점**: 1과 2만으로는 길게 누름의 첫 변화가 100 ms보다 늦어야 한다. `CheckForTap`은 뷰가 `DOWN`을 처리한 뒤 100 ms에 돌고, 그다음 프레임에서 시작한 리플은 몇 프레임 동안 거의 투명하다. 98~105 ms에 보이는 변화는 탭 타임아웃의 리플이 아니거나, 측정의 시작 시각이 게스트 주입 시각과 어긋나 있다. 다른 후보는 3의 호버 해제지만, 그것은 26 ms에 시작해 106 ms쯤 끝나므로 "첫 변화"와 잘 맞지 않는다. 호버 없이(누르기 전에 그 행 위로 움직이지 않고) 길게 누르면 갈린다.

**결정**

- 정적 프레임워크 오버레이로 `config_tapTimeoutMillis`를 0으로 둔다(PR 참조). 대가로, 터치로 목록을 스크롤하기 시작하면 손가락이 터치 슬롭을 넘을 때까지 행이 눌린 것으로 보인다.
- 첫 부팅 dexopt는 바꾸지 않는다. `speed-profile`로 바꾸는 안을 #157에 넣었다가 뺐다. 이미지 빌드가 세어 보니 프로파일을 싣고 온 앱이 0개라서 첫 부팅에 컴파일되는 앱이 없었고, 아래 측정에서 같은 화면을 다시 열어도 클릭 지연이 줄지 않았기 때문이다. 컴파일은 이 지연의 원인이 아니다.
- 시스템 앱 전부를 `speed`로 첫 부팅에 컴파일하는 것도 같은 이유로 하지 않는다.
- 리플의 모양과 속도(2)는 프레임워크 상수라 이미지에서 줄일 수 없다.
- 추적을 더한다. 입력 추적을 켜면, 에이전트가 뗀 지 1초 뒤 그 앱의 `gfxinfo framestats`를 읽어 프레임마다 메인 스레드(`late`, `input`, `animation`, `traversal`, `record`)와 렌더 스레드(`sync`, `issue`, `swap`, `dequeue`, `gpu`) 시간을 `input-trace stage=app.frame` 줄로 남긴다. `dsa-trace.rc`는 모든 앱의 메인 스레드가 16 ms 이상 걸린 메시지를 `Slow dispatch`로 남기게 한다.

**뒤이은 측정 (PC 세션, 소유자 PC, Android 16 이미지, Settings)**

- 클릭에서 화면의 첫 변화까지 101~569 ms, 중앙값 231 ms(13회). 같은 화면을 다시 열어도 줄지 않는다.
- 길게 누르는 동안 앱의 메인 스레드는 대개 2 ms 이하다.
- 그러므로 앱 코드의 실행 시간(컴파일 여부 포함)은 이 지연의 큰 몫이 아니다. 남은 후보는 앱이 그린 프레임이 화면에 닿기까지의 경로(SurfaceFlinger, hwcomposer, 호스트 합성)와 리플이 보이기까지의 시간이다.

**다음 측정에서 확인할 것**

- 오버레이를 넣은 빌드에서 길게 누름의 첫 변화와, 호버 없이 누를 때의 첫 변화.
- `app.frame`의 `late`가 큰 프레임과 같은 때의 `Slow dispatch` 줄: 액티비티 시작의 어느 단계가 메인 스레드를 잡는지.
- 백그라운드 dexopt가 이 VM에서 실제로 도는지(ArtService의 `bg-dexopt` 로그).

## 앱이 프레임을 끝낸 뒤 화면에 닿기까지 (FR-5)

**확인 대상**: feat/touch-latency 8b0558f(#157) 위의 이 브랜치. AOSP SurfaceFlinger의 기본값은 android16-qpr1-release 소스(`VsyncConfiguration`, `FrameTracker`)로 읽었다.

**방법**: 앱이 프레임을 끝낸 뒤 호스트가 그 프레임을 보여 주기까지의 코드를 읽었다. 이 조사에서는 빌드도 실행도 하지 않았다. 아래의 프레임 수는 코드와 기본값에서 계산한 것이고 측정값이 아니다. 측정은 이 PR이 더한 추적으로 한다(`dsa/guest/image/README.md`의 "Tracing a click").

**확인한 것**

1. **SurfaceFlinger의 vsync 설정.** `make-image.py`는 `debug.sf.*`와 `ro.surface_flinger.*`를 하나도 두지 않는다. 그러면 SurfaceFlinger는 작업 시간 방식의 기본값을 쓴다. 앱과 SurfaceFlinger의 작업 시간이 둘 다 vsync 주기 하나이므로, 앱은 화면에 나갈 vsync보다 두 주기 앞에 깨고 SurfaceFlinger는 한 주기 앞에 깬다. 60 Hz에서 앱이 그리기 시작해 SurfaceFlinger가 표시하기까지 최소 33 ms다. 이것은 AOSP 기기와 같은 값이며 잘못된 설정이 아니다. 줄이면 앱이나 SurfaceFlinger가 시간 안에 끝내지 못한 프레임을 놓친다.
2. **hwcomposer의 vsync 스레드.** `VsyncThread`는 `CLOCK_MONOTONIC`의 주기 격자에 맞춰 깨어 콜백을 부르고, 놓친 주기는 건너뛴다. 타임스탬프는 깨어난 시각이 아니라 격자 위의 값이다. 지연을 더하는 곳은 보이지 않는다.
3. **present와 `FramePipeline`.** `submit`은 앞 프레임의 호스트 작업이 끝나기만 기다리고, 이번 프레임은 기다리지 않는다. SurfaceFlinger는 release fence 없이 버퍼를 돌려주므로, 앞 프레임의 버퍼를 호스트가 다 읽기 전에 다음 present가 돌아가면 안 된다. 그러므로 이 대기는 정확성을 위한 것이다. 호스트 왕복(`wm.frame`)이 한 주기보다 짧으면 SurfaceFlinger의 다음 프레임을 막지 않는다. 앞 측정에서 그 왕복은 중앙 0.8 ms, p99 8.6 ms였다.
4. **호스트 창 관리자.** `hosts.rs`는 프레임마다 창 호스트에 알리고, 창 호스트가 공유 텍스처에서 복사를 끝냈다는 답을 기다린다. 쌓이는 큐는 없다.
5. **창 호스트의 스왑체인.** 버퍼 3개, `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`, `Present(0, 0)`이고, 프레임 지연 대기 객체는 쓰지 않는다. flip 모델에서 sync interval 0으로 올린 present는 DWM이 수직 동기마다 하나씩 보여 준다. 한 주기 안에 present가 둘 들어오면 하나가 큐에 남는다. 게스트도 같은 60 Hz로 프레임을 내므로, 한번 쌓인 present는 줄지 않고 프레임마다 한 주기씩 늦게 보일 수 있다(추론). 버퍼가 3개이므로 큐는 많아야 두 프레임, 약 33 ms다.

**결정**

- 코드만으로 확실한 원인은 찾지 못했다. 그래서 이 PR은 고치지 않는다. 1을 줄이는 것과 5에 프레임 지연 대기를 넣는 것은 둘 다 프레임을 놓치거나 렌더러를 막는 대가가 있다. 측정이 그 구간을 원인으로 보여 줄 때 정한다.
- 대신 한 클릭의 전 구간을 호스트 시계 하나에 놓는 추적을 더한다. 호스트의 모든 줄에 성능 카운터 시각(`host=`)을 넣는다. 서비스는 초당 한 번 게스트 데몬과 시계를 맞춰 `clock` 줄을 남긴다. 창 호스트는 스왑체인의 프레임 통계로 실제 표시 시각(`app.shown`)을 남긴다. 에이전트는 SurfaceFlinger의 레이어별 시각(`sf.frame`)을 남긴다. `scripts/input-trace.py`가 이 줄들을 합친다.

**측정으로 답할 질문** (합친 추적의 구간 이름)

- 앱이 그린 프레임이 SurfaceFlinger의 표시까지 몇 주기 걸리는가: `app.frame`의 의도 vsync와 `total`에서 `sf.frame`의 `actual`까지. 두 주기(33 ms)를 크게 넘으면 1의 설정보다 버퍼 대기(`dequeue`)나 BLAST 동기화를 의심한다.
- SurfaceFlinger의 표시에서 hwcomposer의 게시까지: `sf.frame`의 `actual`에서 `hwc.frame`까지. `waited`가 크면 3의 대기가 막고 있는 것이다.
- 호스트 present에서 실제 표시까지: `app.present`에서 `app.shown`까지. 계속 한 주기보다 크면 5의 큐가 쌓인 것이고, 이때 프레임 지연 대기나 `SetMaximumFrameLatency(1)`을 검토한다.
- 누름 뒤 첫 프레임이 언제 그려지는가: `agent.inject`에서 첫 `app.frame`까지. 100 ms 근처면 탭 타임아웃 오버레이가 적용되지 않은 것이다.
- 스왑체인이 프레임 통계를 주지 않으면 창 호스트 로그에 한 번 남는다. 그때는 `app.shown` 대신 외부 캡처로 이 구간을 잰다.

## 클릭 지연 경로 재점검과 Esc, 창 높이 (FR-4, FR-5, FR-16)

**확인 대상**: develop f3be88e. 코드만 읽었고 빌드도 실행도 하지 않았다. 아래 시간은 측정값이 아니다.

**방법**: 호스트 창의 입력(`app_window/pointer.rs`, `keys.rs`)부터 제어 채널(`dsa-channeld`의 `pump`), 에이전트(`ChannelClient`의 `intercept`, `PointerQueue`, `AndroidInputInjector`), hwcomposer(`VsyncThread`, `FramePipeline`), 창 관리자(`hosts.rs`), 창 호스트의 표시(`present.rs`)까지 대기열, 타이머, 폴링, vsync 대기를 찾았다.

**찾은 것**

- 입력 경로에는 지연을 더하는 곳이 없다. `dsa-channeld`의 두 방향은 각자 스레드에서 이벤트로 깨어나고, 20 ms 대기는 멈춤 확인용이라 프레임이 오면 바로 돌아온다. 포인터 프레임은 에이전트의 주 스레드를 거치지 않고 입력 스레드로 바로 가며, 주입은 비동기다. 키는 주 스레드를 거치지만 클릭 지연과는 관계가 없다.
- 창 호스트의 `Present(0, 0)`은 flip 모델의 컴퍼지션 스왑체인이다. sync interval 0에서는 DWM이 다음 vsync 전에 더 새 present가 오면 앞의 것을 버린다(PresentMon의 `Discarded`). 앞 절에서 걱정한 present 큐 누적은 이 경우 생기지 않는다고 본다(추론). 그래서 `SetMaximumFrameLatency`는 넣지 않는다. 넣으면 복사 스레드가 막혀 게스트의 다음 프레임이 늦어진다.
- 남은 큰 몫은 앞 절과 같다: Settings 행의 탭 타임아웃(오버레이로 0), 처음 80 ms쯤 거의 보이지 않는 리플, 클릭 뒤 액티비티 시작과 첫 그리기(약 300 ms). 마지막은 앱이 쓰는 시간이라 입력 경로를 고쳐서 줄지 않는다. 코드로 정당화할 수 있는 새 수정은 찾지 못했다. 측정은 `scripts/input-trace.py`로 `agent.inject`, `app.frame`, `sf.frame`, `hwc.frame`, `app.present`, `app.shown` 구간을 나눠 한다.

**Esc**: 에이전트는 `KEYCODE_ESCAPE`를 그대로 넣었다. 프레임워크의 키 맵(`Virtual.kcm`)은 앱이 처리하지 않은 Escape를 Back으로 바꾸지만, 그 대체 키는 앱이 키를 거절한 뒤 입력 디스패처가 만들며, 주입한 키에서 일어나지 않을 수 있다(실측에서 Settings 하위 페이지가 돌아가지 않았다). 이제 수정 키 없는 Esc는 에이전트가 처음부터 `KEYCODE_BACK`으로 넣는다. 대가로 Escape를 직접 보는 앱은 수정 키 없는 Esc를 받지 못한다. Shift, Ctrl, Alt, Meta와 함께 누른 Esc는 Escape로 남는다.

**창 높이**: 게스트 디스플레이는 모니터 기록이 있으면 가장 긴 변의 정사각형이지만, 기록이 없으면 이미지 기본값 1920x1080이 남았다. 태스크는 디스플레이보다 클 수 없으므로 그때 창 높이가 1080에서 멈춘다. 이제 기록이 없어도 1920 정사각형으로 둔다.

## R6. 커널 명령줄에서 같은 `androidboot` 값을 두 번 주면 어느 쪽이 이기는가 (FR-4)

**배경**: 게스트 디스플레이 크기를 호스트가 정하려면, 이미지의 `kernel_cmdline`에 이미 있는 `androidboot.dsa.display_width` 뒤에 값을 덧붙이면 되는지 확인해야 했다.

**확인 방법**: AOSP `system/core/init/property_service.cpp`(`android16-release`)를 읽었다. 실제 부팅으로는 확인하지 않았다.

**결과**:
- `PropertyInit`은 `ProcessKernelDt`, `ProcessKernelCmdline`, `ProcessBootconfig` 순으로 부른다.
- `PropertySet`은 `ro.*` 속성을 한 번만 쓰게 하고, 다시 쓰면 "Read-only property was already set"으로 거절한다.
- 따라서 **앞의 값이 이긴다**. 뒤에 덧붙인 값도, bootconfig의 값도 이미지의 명령줄 값을 덮지 못한다.

**결정**: 호스트는 이미지의 너비와 높이 단어를 지우고 자기 값을 넣는다(`dsa_host_core::guest_display::with_display_size`).

## 게스트 로그를 채우던 두 오류: wakeup 통계와 cgroup 이동 (FR-5, FR-10)

**확인 대상**: 안드로이드 16 이미지(`aosp_cf_x86_64_only_phone` 15581820), WSL 커널 `linux-msft-wsl-6.18.54.1`(`config-wsl`, `kernel/cgroup/cgroup-v1.c`, `drivers/base/power/wakeup_stats.c`). 실제 PC의 콘솔 로그(54,227줄, 2026-11-01)와 이미지의 `plat_sepolicy.cil`, `cgroups.json`, `task_profiles.json`, `init.rc`를 읽었다. 고친 커널은 이 기기에서 부팅해 보지 않았다.

### `Error opening wakeupN/<stat> for wakeupN: Permission denied`

- **무엇이었나**: `android.system.suspend-service`가 `/sys/class/wakeup/wakeup0`부터 `wakeup4`까지 다섯 개의 통계 파일을 열 때마다 거절당했다. 감사 로그는 모두 `scontext=u:r:system_suspend:s0 tcontext=u:object_r:sysfs:s0 tclass=file`이다(이 로그의 거절 336건 중 203건).
- **원인**: 플랫폼 정책은 `/class/wakeup`과 `/sys/devices/virtual/wakeup`만 `sysfs_wakeup`으로 붙인다. `/sys/class/wakeup/wakeupN`은 링크이고, 장치가 가진 wakeup 소스의 실제 노드는 그 장치 아래(`<장치>/wakeup/wakeupN`)에 있다. genfscon은 실제 경로로 붙으므로 그 노드는 그냥 `sysfs`다. Cuttlefish와 goldfish는 자기 장치 경로를 vendor 정책에 나열하지만, Hyper-V 게스트의 장치는 VMBus 인스턴스 GUID와 호스트의 ACPI 표(`rtc_cmos PNP0B00:00`, `alarmtimer`, `hyperv_keyboard` 등)로 이름이 정해져 호스트 버전에 따라 바뀐다. 고정된 경로 목록으로는 붙일 수 없다.
- **고친 것**: `dsa/kernel/patches/wakeup-sources-virtual.patch`가 wakeup 소스 장치를 부모 없이 만든다. 그러면 모든 wakeup 소스가 `/sys/devices/virtual/wakeup/wakeupN`에 생기고, 플랫폼 정책이 이미 `sysfs_wakeup`으로 붙인다. 잃는 것은 소스 노드의 `device` 링크뿐이고, 장치의 `power/wakeup` 파일은 그대로다. `check-image.py`는 정책에 두 genfscon이 없으면 이미지를 거절한다.
- **다른 sysfs**: `/sys/power/wakeup_count`(`sysfs_power`)와 `/sys/power/suspend_stats`(`sysfs_suspend_stats`)는 플랫폼 정책에 있고 `CONFIG_PM_SLEEP=y`라 존재한다. `/sys/kernel/wakeup_reasons`는 안드로이드 공통 커널에만 있어 부팅에 한 번 "No such file"이 남는다. 반복되지 않으므로 그대로 둔다.

### `libprocessgroup: Failed to add task into cgroup`

- **무엇이었나**: 이 줄 바로 앞에 `AddTidToCgroup failed to write '<pid>'; path=foreground|background|top-app|; process: Permission denied`가 있다(237건). 쓰는 쪽은 system_server(`SchedulingPolicyService`, `ActivityManager`)다. SELinux 거절은 없다.
- **원인**: 커널 설정에는 빠진 컨트롤러가 없다. `config-wsl`에 `CGROUPS`, `CPUSETS`, `CGROUP_SCHED`, `FAIR_GROUP_SCHED`, `BLK_CGROUP`, `CGROUP_FREEZER`, `MEMCG`가 있고 `UCLAMP_TASK(_GROUP)`은 DSA가 켠다. 이미지의 `cgroups.json`은 blkio, cpu, cpuset을 v1으로(`/dev/blkio`, `/dev/cpuctl`, `/dev/cpuset`), freezer와 memory를 v2(`/sys/fs/cgroup`)로 올린다. 막힌 곳은 v1의 `cgroup.procs` 쓰기 권한이다. 업스트림 `__cgroup1_procs_write`는 root이거나 대상 태스크와 같은 사용자일 때만 옮기게 한다. system_server(uid 1000)가 다른 uid의 앱을 옮기면 `EACCES`다. 안드로이드 공통 커널은 여기에 `CAP_SYS_NICE` 예외를 더한다.
- **고친 것**: `dsa/kernel/patches/cgroup-v1-sys-nice.patch`가 같은 예외를 넣는다. 파일을 연 쪽의 자격(`file_ns_capable(of->file, ...)`)으로 확인하므로 상속된 fd로 우회할 수 없다. 앱이 foreground와 top-app의 cpu, cpuset 그룹에 들어가므로 포그라운드 앱의 스케줄링 힌트(uclamp)도 다시 적용된다. `android.config`는 위 컨트롤러를 이름으로 적어, `config-wsl`이 하나를 빼면 빌드가 실패한다.
- **memory.reclaim**: memory 컨트롤러는 v2에 그대로 있고 이 패치는 v1 쓰기만 바꾼다. `dsa-channeld.rc`의 `/sys/fs/cgroup/memory.reclaim` 쓰기는 영향을 받지 않는다.
- **`ServiceCapacityLow`**: `dsa-hwcomposer.rc`의 이 프로필은 cpuset `system-background`에 들어가는 것이다. `init.rc`가 이 그룹에 모든 CPU를 복사하므로 컴포저가 쓸 CPU는 줄지 않는다. AOSP의 컴포저 서비스(ranchu 등)도 같은 줄을 쓴다. init이 root로 쓰므로 이번 오류와도 관계가 없다. 그대로 둔다.

**실제 PC에서 확인할 것**: 위 두 로그 줄이 사라지는지, `cat /proc/cgroups`에 cpu, cpuset, blkio, memory, freezer가 켜져 있는지, `ls -Z /sys/class/wakeup/`의 대상이 `/sys/devices/virtual/wakeup/`이고 `sysfs_wakeup`인지.

## 부팅 로그의 시스템 서비스 오류, 감사 로그 홍수, 낡은 패키지 상태 (FR-1, FR-6, FR-14)

**확인 대상**: 안드로이드 16 이미지(`aosp_cf_x86_64_only_phone` 15581820, `16.0.0-15581820-7`), develop `dcbb771`. 실제 PC의 콘솔 로그(`android-<SID>.log`, 14,937줄, 2026-11-04, 부팅 두 번)와 같은 커밋의 `DsaAgent.apk`(`resources.arsc`, 바이너리 매니페스트)를 읽었다. 고친 이미지는 이 기기에서 부팅해 보지 않았다.

**결과와 결정**:

| 로그 | 건수 | 누가, 왜 | 결정 |
| --- | --- | --- | --- |
| `E SystemServiceRegistry` `dropbox` | 12건(줄 약 240) | system_server가 부팅 초기의 WTF를 DropBox에 넣으려 했지만 `DropBoxManagerService`가 4 ms 뒤에 시작했다. | 무해. AOSP의 시작 순서이고 DSA가 뺀 서비스가 아니다. 서비스는 곧 떠서 이후 기록은 들어간다. |
| `carrier_config` | 10건 | Wi-Fi(`WifiServiceImpl`), 시간대 감지 등이 `CarrierConfigManager`를 찾는다. 이미지에 전화 기능(rild)이 없다. | 무해. 호출하는 쪽이 null을 처리한다. WSA에도 전화 기능은 없었다. |
| `jobscheduler` | 2건 | `SettingsProvider.scheduleWriteFallbackFilesJob`이 JobScheduler보다 먼저 불렸다. | 무해. 시작 순서. |
| `uwb`, `thread_network` | 각 1건 | 연결 서비스의 초기화. UWB와 Thread HAL APEX는 DSA가 뺐다(`REMOVED_APEXES`). | 무해. 한 번만 묻고 기능이 없다고 판단한다. WSA에도 없었다. |
| `trust`, `virtualdevice`, `supervision`, `keyguard` | 각 1건 | `SystemServer.startOtherServices`, `ProtectedPackages.isSupervisionPackage`가 서비스 시작 전에 매니저를 만들었다. | 무해. 시작 순서. |
| `E InputMethodManagerService: Meta-data does not start with input-method tag` | 1건 | 아래 "낡은 패키지 상태". 호스트 입력기가 등록되지 않아 텍스트 입력이 안 된다. | 고침: 이미지마다 지문을 바꾼다. |
| `W DsaAgent` `MANAGE_ROLE_HOLDERS`, `MANAGE_NOTIFICATION_LISTENERS` 거절 | 각 1건 | 같은 원인. 새 에이전트가 요청한 권한이 부여되지 않았다. | 고침: 같은 변경. |
| `W DsaAgent` `User 0 must be unlocked for widgets` | 3건 | 에이전트는 사용자 저장소 잠금 해제 전에 시작하는데, 위젯 호스트는 그 전에 시작할 수 없다. 에이전트는 다시 시도하지 않아 그 부팅 내내 위젯이 그려지지 않았고, 연결 때 읽지 못한 목록을 빈 목록으로 보내 호스트가 배치한 위젯을 지울 수 있었다. | 고침: 시작이 거절되면 1초마다 다시 시도하고(경고는 한 번), 시작하기 전에는 위젯 목록을 보내지 않는다. |
| `W auditd`, `kauditd hold queue overflow` | 705줄, 손실 1,058건 | 커널 명령줄의 `audit=1`. 커널이 iptables 변경(`NETFILTER_CFG`), BPF 프로그램 적재(`BPF`), 각 거절의 `SYSCALL`, `PROCTITLE`, `EOE` 기록까지 남겼고, logd가 뜨기 전에 대기열(64)이 넘쳤다. | 고침: `audit=1`을 뺀다. 안드로이드 기기도 주지 않는다. SELinux 거절은 그대로 logd의 auditd로 간다. |
| `avc: denied { write } kdevtmpfs` | 부팅당 70여 건 | 커널의 devtmpfs 스레드가 정책 적재 뒤 찾은 장치마다 노드를 만들려 했다. 안드로이드는 이 devtmpfs를 올리지 않는다(`/dev`는 init의 tmpfs, 노드는 ueventd). | 고침: 거절은 두고 기록만 하지 않는다(`sepolicy/kernel.cil`의 `dontaudit`). |
| `avc: denied { set }` `ro.debuggable`, `ro.secure`, `ro.adb.secure`, `ro.force.debuggable`, `sys.use_memfd` | 부팅당 10건 | `edit_props`가 이 값을 vendor와 odm의 build.prop에도 썼다. init은 두 파일을 vendor_init으로 읽고, vendor_init은 이 속성을 쓸 수 없다. 값은 system build.prop에서 이미 들어갔다. | 고침: vendor와 odm build.prop에는 쓰지 않고, 있던 줄도 지운다. `check-image.py`가 확인한다. |
| `W iptables-restor` | 123줄 | netd의 iptables 적재를 `audit=1`이 감사 기록으로 남긴 것. | 고침: `audit=1` 제거로 사라진다. |
| `W PackageManager` | 253줄 | 대부분 APEX 패키지(`com.android.vndk.v*`, `com.google.cf.*`)의 "Skipping preparing app data"와 첫 부팅의 "Missing permission state". | 무해. AOSP 기본 동작이다. |
| `W Zygote: Class not found for preloading` | 150줄 | 프레임워크의 preloaded-classes 목록이 R8 합성 클래스 이름과 맞지 않는다. | 무해. AOSP 빌드 그대로이고 기능에 영향이 없다. |
| 나머지 거절(`discard_max_bytes`, `storaged` sysfs, `init_dev_config`, 센서 HAL memfd, `crash_dump` keystore) | 각 1~4건 | Cuttlefish 장치 스크립트와 AOSP의 알려진 거절(`bug=` 표시). | 그대로 둔다. 반복되지 않고 기능에 영향이 없다. |

### 낡은 패키지 상태

- **무엇이었나**: 매니페스트의 `android.view.im` 메타데이터는 `0x7f030001`(`res/rz.xml`, `<input-method>`)을 가리키고 APK 자체는 맞다. 그런데 system_server는 그 자원을 `<input-method>`가 아니라고 읽었다. 같은 부팅에 `Checksum mismatch for dex DsaAgent.apk`도 있다.
- **원인**: 이미지의 지문이 AOSP 빌드의 것(`.../15581820:user/release-keys`) 그대로라, DSA가 이미지를 새로 만들어도 바뀌지 않는다. PackageManager는 파티션 지문의 요약(`PackagePartitions.FINGERPRINT`)이 바뀔 때만 업그레이드로 보고 파싱 캐시를 버리고, 다시 컴파일하고, 새 권한을 준다. 그래서 사용자 데이터에 남은 이전 에이전트의 파싱 결과를 썼다. 이전 에이전트에서는 `0x7f030000`이 입력기 XML이었고, 접근성 서비스가 들어온 지금은 그 번호가 접근성 XML이다.
- **고친 것**: `make-image.py`가 이미지 내용(DSA가 넣는 파일과 `dsa/guest/image/`의 파일)의 요약으로 `dsa<12자리>` 태그를 만들어, 모든 build.prop의 `ro.*build.version.incremental`과 `ro.*build.fingerprint`의 incremental 부분에 붙인다. 모든 파티션에 같은 태그가 붙어 서로 같다. 새 이미지의 첫 부팅은 OTA 뒤처럼 앱을 다시 컴파일하므로 조금 길어진다.

**실제 PC에서 확인할 것**: 새 이미지로 부팅한 뒤 `getprop ro.build.fingerprint`에 `.dsa`가 붙는지, 로그에 `Unable to load input method`가 없고 `ime list -s`에 `io.github.thisisthepy.dsa.agent/.platform.HostInputMethod`가 있는지, 호스트에서 한글을 입력할 수 있는지, `DsaAgent`의 권한 거절이 없는지. `kauditd hold queue overflow`, `type=1325`, `type=1334`, `kdevtmpfs` 거절, `vendor_init`의 `{ set }` 거절이 사라졌는지.

## 클릭 지연과 게스트 처리기 성능 (FR-5)

**확인 대상**: develop dcbb771이 설치된 소유자 PC(Intel Arc, Settings 앱, 20 ms 클릭 15회, 1픽셀 임계값의 연속 화면 캡처, `scripts/input-trace.py`로 합친 추적). 이미지 16.0.0-15581820-7의 `signed-target_files.zip`. AOSP ART Service의 `ArtManagerLocal.getDefaultPackages`.

**실측**

- 호스트: `app.pointer`에서 `app.present`까지 13~25 ms, present에서 표시까지 한 번의 새로 고침 안(`app.shown exact=true`가 대부분).
- 클릭에서 첫 화면 변화까지 p50 252 ms, p90 302 ms, 최대 488 ms(11회). 첫 변화는 언제나 새 화면 전체(300x80 캡처에서 약 23,000픽셀)였고, 누름 표시나 리플은 한 번도 잡히지 않았다.
- 누름과 뗌 직후 앱 프레임 2~3개(순번이 오른다)에는 보이는 변화가 없고, 그 뒤 131~351 ms 동안 앱 프레임이 없다. 그동안 메인 스레드는 `SubSettings`를 시작한다(`Displayed SubSettings +106..+326ms`).
- 서비스 로그: 부팅 직후 `power: running Android's processors frugally`가 한 번 나오고, 12:14~12:16 UTC의 클릭 동안 `full speed` 줄이 없다.

**찾은 것**

1. **로그만으로는 클릭 중 성능을 알 수 없었다.** `guest_performance::follow`는 성능마다 VM당 처음 한 번만 로그를 남겼다. 그래서 `full speed` 줄이 없다는 것은 부스트가 오지 않았다는 증거가 못 된다. 다만 부스트가 왔더라도, 부스트가 끝나면 frugal로 돌아갔다.
2. **창이 보이는 동안에도 frugal이 될 수 있었다.** 정책은 `LOW_POWER`, `DEVICE_IDLE`, `DISPLAY_INACTIVE` 중 하나가 켜져 있고 성능 모드나 부스트가 없으면 frugal(EcoQoS, 낮은 우선순위)이었다. 호스트가 셸이라 게스트의 기본 디스플레이는 사용자가 보는 화면이 아니며, 이 모드들은 작업 창이 보이는 것과 무관하게 켜져 있을 수 있다. 실측의 "부팅 직후 frugal, 그 뒤 변화 없음"과 맞는다.
3. **호스트는 클릭을 Android보다 먼저 안다.** 게스트의 `INTERACTION` 부스트는 입력이 게스트에 닿고, 입력 디스패처와 전원 관리자를 거친 뒤 제어 채널로 돌아온다. 그동안 vCPU는 frugal 상태로 입력을 처리한다.
4. **Settings는 컴파일되지 않은 채로 돈다.** 이미지의 Settings odex/vdex는 `SYSTEM_OTHER/system_ext/priv-app/Settings/oat/x86_64/`에만 있고, DSA 디스크는 system_other를 싣지 않는다. 게다가 `make-image.py`가 블루투스 때문에 `Settings.apk`의 dex를 고치므로, 그 odex를 실어도 dex 체크섬이 맞지 않는다. APK 안에는 기준 프로파일(`assets/dexopt/baseline.prof`)이 없다. 첫 부팅 dexopt는 모든 패키지를 `pm.dexopt.first-boot=verify`로 처리하므로 Settings는 검사만 되고 인터프리터와 JIT로 돈다. 이것은 앞 절의 `CompilerFilter=verify` 관찰의 원인이다.
5. **vCPU와 cgroup**: 기본 vCPU는 4개(`host_config.rs`)다. top-app cpuset과 uclamp는 앞 절의 커널 패치(cgroup v1 쓰기 예외) 뒤로 다시 적용된다. 이번 빌드의 게스트에서 `/dev/cpuset/top-app/cpus`는 확인하지 않았다.
6. **누름 프레임이 비어 보이는 이유**: 앞 절의 2대로 패턴 리플은 `setPressed` 뒤 33 ms에 알파 5%, 반지름 0.7%, 67 ms에 25%, 3.2%다. 누름과 뗌 직후의 2~3프레임은 이 구간에 들어가므로, 리플은 클릭 지점의 몇 픽셀에 거의 투명하게 그려진다. 그다음 프레임들은 메인 스레드가 액티비티를 시작하는 동안 나오지 않는다. 리플 진행이 메인 스레드의 애니메이터에 묶여 있다는 뜻이다(추론, 게스트에서 확인하지 않음). DSA의 hwcomposer와 호스트가 레이어를 버리거나 합친 흔적은 없다: 프레임 순번이 오르고 호스트가 present를 했다.

**고친 것**

- 작업 창이 하나라도 보이면 frugal로 가지 않는다(`PowerHints::windows_shown`). frugal은 창이 없고 게스트가 frugal 모드를 말할 때만이다.
- 호스트가 누름, 뗌, 스크롤, 키를 게스트로 보낼 때 그 자리에서 1.5초 부스트를 건다(`Session::pointer`, `Session::key`가 `GuestPower::HostInput`을 낸다). 감시 스레드는 채널에서 깨어나 바로 적용하므로 폴링이 없다. 게스트의 `LAUNCH` 모드와 `INTERACTION` 부스트도 같은 경로로 바로 적용된다.
- 성능이 바뀔 때마다 UTC 시각, 원인, 받은 뒤 적용까지 걸린 시간을 로그에 남긴다.

**하지 못한 것과 이유**

- **이미지 빌드에서 Settings 컴파일**: 고친 dex로 odex를 만들려면 호스트용 `dex2oat`가 필요하다. 이 프로젝트가 받는 `otatools.zip`과 target_files에는 없고, ART 호스트 도구를 새로 받거나 빌드하는 것은 소유자에게 먼저 물어야 하는 새 도구다. 첫 부팅의 `pm.dexopt.first-boot`를 `speed`로 올리면 101개 APK(312 MiB)를 모두 첫 부팅에 컴파일하고, `speed-profile`은 프로파일이 없는 Settings에는 `verify`와 같다.
- **누름 리플을 앞당기는 것**: 리플의 진입 곡선은 프레임워크의 컴파일 시점 상수라 이미지 오버레이로 바꿀 수 없다.

**제안 (SPEC은 바꾸지 않았다)**

1. ART 호스트 도구(`dex2oat64`, 부트 이미지 포함)를 `.scratch/`로 받는 단계를 `fetch.sh`에 더하고, 이미지 빌드에서 고친 Settings를 `speed`로 컴파일해 `oat/`를 같이 싣는다. 비용: 이미지 빌드 도구 하나와 수 MB. 얻는 것: 첫 실행부터 컴파일된 Settings. 소유자의 도구 승인이 필요하다.
2. 대안으로, 에이전트가 부팅 완료 뒤 한 번 시스템 서버에 Settings의 `speed` dexopt를 요청하는 vendor 서비스를 둔다. `cmd package compile`은 root나 shell만 부를 수 있으므로 별도 SELinux 도메인과 init 서비스가 필요하다.
3. 누름 표시는 FR-5의 수용 기준을 "클릭 뒤 첫 시각 변화"로 볼지 "새 화면"으로 볼지를 소유자가 정해야 한다. 리플은 Android가 의도적으로 처음 수십 ms 동안 거의 투명하게 그린다.

**실제 PC에서 확인할 것**

- 창을 연 뒤 서비스 로그에 `power: <UTC> running Android's processors as any program (..., a window shown; took N us)`가 나오고, frugal은 창을 모두 닫은 뒤에만 나오는지.
- 클릭마다 `power: <UTC> running Android's processors at full speed (the user's input, ...; took N us)`가 누름 시각(`app.pointer`)과 1 ms 안에 나오고, 약 1.5초 뒤 `as any program`으로 돌아오는지.
- 같은 15회 클릭에서 첫 화면 변화의 p50과 p90, `Displayed SubSettings`의 시간이 줄었는지.

## 뒤로 버튼의 호버 표시가 왼쪽으로 어긋나 보이는 문제 (FR-5)

**확인 대상**: 소유자 보고(Windows 11, 2880x1800 모니터 200%, Intel Arc, develop dcbb771). Settings 창의 Network & internet에서 툴바의 뒤로 화살표 위에 마우스를 올리면 호버 표시가 실제 버튼보다 조금 왼쪽에 보인다. 입력 좌표가 어긋났다는 가설(dp, DPI, 창 테두리)을 포인터 경로 전체를 읽어 검증했다. 같은 빌드에서 클릭 지연을 잴 때 찍은 `SetWindowPos(300, 150, 900, 1100)` 창 캡처(`.scratch/test-dcbb771/touch1/opened-network.png`)를 픽셀 단위로 읽었다.

**경로 (코드를 읽은 결과)**

1. **Win32 메시지**: `dsa-app-window`의 창 스레드는 창을 만들기 전에 `PER_MONITOR_AWARE_V2`를 켠다(`app_window/window.rs`). `WM_MOUSEMOVE`와 버튼 메시지의 `lParam`은 클라이언트 좌표의 실제 픽셀이고, 휠과 `WM_POINTER*`는 화면 좌표를 `ScreenToClient`로 바꾼다(`app_window/pointer.rs`). 창 테두리, DWM의 보이지 않는 크기 조절 테두리, 캡션은 클라이언트 밖이라 매핑에 들어오지 않는다. 창 직사각형(`GetWindowRect`)을 쓰는 곳은 포인터 경로에 없다.
2. **호스트 매핑**: `WindowGeometry::to_guest`는 클라이언트 좌표에 `작업 크기 / GetClientRect 크기`를 곱한다. 창의 클라이언트는 작업 크기와 같게 맞춰지므로(`apply_size`, 게스트 픽셀 하나가 창 픽셀 하나) 정상 상태의 비율은 정확히 1이다. 호버(`MouseMove`)와 클릭(`MouseButton`)은 같은 `message_point`와 같은 `to_guest`를 지난다.
3. **에이전트 주입**: `PointerInput`은 호버와 누름 모두 `bounds.left + x`, `bounds.top + y`에 `SOURCE_MOUSE`로 주입한다. `bounds`는 에이전트가 호스트에 알린 작업 경계이고, hwcomposer도 같은 `TaskUpdated`로 작업 경계를 받아 레이어를 그 원점 기준으로 합성한다(`compose_request.cpp`의 `relativeTo`). 입력과 화면의 원점이 같은 값에서 나온다.
4. **화면 쪽**: 각 프레임은 대상 버퍼의 왼쪽 위 작업 크기 영역을 채우고(gfxstream `CompositorVk`는 대상 이미지 크기로 정규화한다), 호스트는 `SetSourceSize`와 비주얼 배율(`visual_scale`)로 그 영역을 클라이언트 전체에 맞춘다. 자르기나 오프셋은 없다.

**실측 (캡처의 픽셀)**

- 카드의 왼쪽 끝은 x=45, 오른쪽 끝은 x=855다. 보이는 창 테두리는 12와 888이므로 양쪽 여백이 33픽셀로 같다. 화면이 가로로 밀려 그려지지 않는다.
- 뒤로 버튼의 원형 배경은 x 61~140, y 74~153(중심 100.5, 113.5)이고, 화살표는 x 85~116, y 98~128(중심 100.5, 113)이다. 원과 화살표의 중심이 같다.

**찾은 것**

1. **호스트, 프로토콜, 에이전트의 포인터 경로에는 일정한 오프셋도 비율 오차도 없다.** 정상 상태에서 클라이언트 픽셀 (x, y)는 작업 픽셀 (x, y)에 정확히 닿는다. 호버와 클릭은 같은 변환을 지나므로 서로 다를 수 없다.
2. **호버 표시의 위치로는 입력 오프셋을 알 수 없다.** 호버 상태는 포인터 아래의 뷰가 자기 경계에 그린다. 입력이 몇 픽셀 어긋나면 표시가 켜지는 커서 위치가 달라질 뿐, 표시가 그려지는 자리는 그대로다. 그러므로 "표시가 버튼보다 왼쪽에 그려진다"는 관찰은 입력 경로가 아니라 앱이 그린 것이다.
3. **(아래 "재조사"가 대체한다.) 남은 후보는 Settings의 그리기다(추론, 게스트에서 확인하지 않음).** Android 16 Settings의 툴바 뒤로 버튼은 원형 배경이 따로 있고, 호버 상태 레이어는 버튼 뷰(최소 56dp 폭) 전체나 리플의 핫스팟 영역에 그려진다. 둘의 중심이 다르면 휴대폰에서 마우스를 써도 같은 모습이 된다. 확인하려면 게스트에서 `adb shell uiautomator dump`나 레이아웃 검사기로 뒤로 버튼 `ImageButton`의 경계와 원형 배경의 경계를 비교한다.
4. **늘어난 프레임에서의 작은 편향**: 크기 조절 중 게스트가 새 크기를 그리기 전에는 이전 프레임이 늘어나 보이고, 매핑은 같은 비율을 따른다. 매핑은 픽셀 중심이 아닌 픽셀 인덱스를 비율로 곱하므로 늘린 비율 r에서 최대 r/2 게스트 픽셀만큼 왼쪽 위로 치우친다. 1:1인 정상 상태에서는 0이고, 한 픽셀 미만이라 보고된 증상의 원인이 아니다.

**고친 것**

- 코드 결함을 찾지 못했으므로 동작은 바꾸지 않았다. 대신 실제 기하로 회귀를 막는 테스트를 더했다. `fr5_at_200_percent_every_client_pixel_lands_on_the_same_task_pixel`(200%, 869x1031처럼 320 dpi에서 정수 dp가 아닌 크기의 모든 클라이언트 픽셀), `fr5_at_200_percent_a_hover_and_a_click_on_the_back_button_land_on_the_same_point`, `fr5_a_frame_stretched_by_a_non_integer_ratio_has_no_offset_that_grows_across_it`(호스트), `fr5_a_hover_and_a_click_at_one_point_reach_the_same_display_point`(에이전트).

**실제 PC에서 확인할 것**

- 뒤로 버튼 위에서 커서를 천천히 왼쪽에서 오른쪽으로 옮기며 호버 표시가 켜지는 x와 꺼지는 x를 적는다. 그 구간이 화살표 중심에 대해 좌우 대칭이면 입력은 맞고, 차이는 앱의 그리기다.
- 게스트에서 `adb shell settings put system pointer_location 1`로 포인터 위치 표시를 켠다. 이 표시는 시스템 서버가 그리는 오버레이라 SystemUI가 없어도 나온다. 주입된 호버의 십자선이 Windows 커서 끝과 겹치면 입력은 맞다. 끝나면 `0`으로 끈다.

### 재조사: 원인은 SettingsLib 뒤로 아이콘의 비대칭 배치다 (FR-4, FR-5)

소유자는 "앱이 그린 것이라 우리 문제가 아니다"라는 결론을 받아들이지 않았다. 호버 표시와 클릭 리플이 모두 그려진 화살표보다 왼쪽에 있고, dp나 밀도 문제를 의심했다. 이 절은 그 가설을 숫자로 검증하고 원인을 고친 기록이다.

**실측 (PC, Android 16 x86_64 게스트, Settings > 네트워크 및 인터넷, 디스플레이 2의 freeform 작업)**

- 작업 경계 `Rect(1166, 953 - 1989, 2416)` = 823x1463, 창 클라이언트도 823x1463(1:1).
- `dumpsys activity`: 전역 구성은 2880x2880 디스플레이에서 `sw2163dp 213dpi`, 작업의 재정의 구성은 `sw412dp w412dp h732dp 320dpi`(호스트가 `WindowContainerTransaction.setDensityDpi`로 작업마다 밀도를 준다).
- uiautomator(작업 좌표): `action_bar`는 x 16..823, 내비게이션 `ImageButton`은 x 16..128, y 0..112(112px = 320 dpi의 56dp). 목록 글자는 노드 좌표 그대로 그려진다("인터넷" 노드 x=168, 클라이언트 x 168). 전체 이동은 없다.
- 스크린샷(클라이언트 좌표): 뒤로 버튼의 원형 배경은 x 47..127(80px, 중심 87), 화살표 중심도 87이다. 뷰의 중심은 72이고, 호버 레이어와 리플은 72 둘레에 그려진다. 차이는 약 15px이다.

**SettingsLib 소스 (frameworks/base android16-release)**

- `CollapsingToolbarBaseActivity/res/layout-v36/settingslib_expressive_collapsing_toolbar_content_layout.xml`: 툴바는 프레임워크 `Toolbar`이고 `layout_marginStart="@dimen/settingslib_expressive_space_extrasmall4"`(8dp)를 둔다. uiautomator의 x=16(320 dpi의 8dp)과 맞다.
- `CollapsingToolbarDelegate.java`: `actionBar.setHomeAsUpIndicator(R.drawable.settingslib_expressive_icon_back)`.
- `CollapsingToolbarBaseActivity/res/drawable-v36/settingslib_expressive_icon_back.xml`: `layer-list`다. 첫 항목은 `android:start="16dp"`에 40x40dp 원, 둘째 항목은 `android:start="16dp"`, `gravity="center"`인 16dp 화살표다. 끝 쪽 인셋은 없다. 그래서 그림의 고유 폭은 56dp이고, 원은 그림 안에서 16..56dp, 중심은 36dp다. 그림의 중심(28dp)보다 8dp 오른쪽이다.
- 내비게이션 버튼은 프레임워크 `Toolbar`의 `ImageButton`이다(`Widget.Material.Toolbar.Button.Navigation`: 최소 폭 56dp, 배경 `?selectableItemBackgroundBorderless`). 경계 없는 리플과 호버 상태 레이어는 뷰의 중심 둘레에 그려진다.

**숫자 대조**

- 320 dpi에서 원은 버튼 x 16에서 16dp(32px) 들어간 48..128, 중심 88이다. 측정값 47..127, 중심 87과 1px 안에서 맞는다. 원 폭 80px = 40dp도 320 dpi다.
- 리플 중심은 16 + 56px = 72다. 측정값 72와 맞는다.
- 두 중심의 차이는 8dp = 320 dpi의 16px이다(측정 15px, 픽셀 경계 반올림). 213 dpi로 풀었다면 원 폭은 53px, 차이는 10.7px이어야 한다. 240 dpi라면 60px과 12px이다. 측정과 맞는 것은 320 dpi 하나뿐이다.
- 따라서 원, 화살표, 버튼, 리플은 모두 작업의 320 dpi로 해석되어 있다. 전역 213 dpi로 풀린 자원도, 밀도가 섞인 인셋도 없다. 어긋남은 아이콘 자체가 원을 버튼 중심에서 8dp 오른쪽에 그리기 때문이고, 밀도와 무관하게 모든 밀도에서 같은 비율로 생긴다. 휴대폰에서는 터치 리플이 손가락 아래에서 퍼지고 호버가 없어 눈에 덜 띌 뿐이다.

**전역 밀도와 작업 밀도의 불일치에 대한 판단**

- 디스플레이 밀도(213)와 작업 재정의(320)의 차이는 이 증상의 원인이 아니다. 위 숫자에 213 dpi의 흔적이 없다.
- 앱 프로세스의 구성은 그 프로세스의 활동이 속한 작업의 구성을 따른다(`WindowProcessController`가 활동의 구성에 등록된다). 그래서 `Application` 컨텍스트의 자원도 보통 320 dpi로 풀린다. 한 프로세스가 밀도가 다른 두 창에 활동을 두는 경우만 예외가 될 수 있다.
- 디스플레이 밀도를 `wm density`로 맞추는 방식으로 바꾸지 않는다. 한 게스트 디스플레이에 여러 호스트 모니터의 창이 함께 있고, 창마다 자기 모니터의 밀도가 필요하다. 이것은 프레임워크의 데스크톱 창 모드가 쓰는 작업별 밀도와 같은 방식이다. 디스플레이 하나의 밀도로는 모니터마다 다른 배율을 줄 수 없다.
- 다른 앱에서 같은 불일치가 보이는지는 이 조사에서 PC로 확인하지 않았다(백그라운드 작업이라 기기를 쓰지 않는다). 확인 방법: 앱 창 하나에서 `adb shell dumpsys activity activities`의 작업 구성 dpi와, 그 앱 프로세스의 `adb shell dumpsys activity processes`에 나온 구성 dpi가 같은지 본다.

**고친 것**

- 이미지에 Settings용 정적 오버레이 `/vendor/overlay/DsaSettingsOverlay.apk`(`io.github.thisisthepy.dsa.overlay.settings`, 대상 `com.android.settings`)를 더했다. `settingslib_expressive_icon_back`을 원과 화살표 양쪽에 8dp씩 둔 그림으로 바꾼다. 고유 폭은 그대로 56dp라 버튼과 제목의 자리는 변하지 않고, 원의 중심이 버튼의 중심에 온다. 호버와 리플이 원과 같은 중심에 그려진다. 원은 휴대폰 디자인보다 8dp 왼쪽(창 끝에서 24dp가 아닌 16dp)에 놓인다.
- 색은 SettingsLib의 v34 값과 같은 `system_surface_container_highest_light/dark`, `system_on_surface_variant_light/dark`를 `drawable`과 `drawable-night`로 나눠 쓴다. 오버레이는 프레임워크 오버레이처럼 aapt2로 이미지의 framework-res에 맞춰 만들고 플랫폼 키로 서명한다.
- `check-image.py`는 이 오버레이가 없는 이미지를 거부한다. 테스트: `test_fr5_settings_back_button_circle_is_centred_on_the_button_its_hover_is_drawn_around`, `test_fr5_the_settings_overlay_is_built_against_the_images_own_framework`, `test_fr5_check_image_rejects_an_image_without_them`.

**PC에서 확인할 것**

- 새 이미지에서 `adb shell cmd overlay list com.android.settings`에 `[x] io.github.thisisthepy.dsa.overlay.settings`가 보이는지 본다.
- 같은 화면에서 원형 배경이 x 32..112(중심 72)에 그려지고, 호버와 리플의 중심이 원의 중심과 같은지 스크린샷으로 본다.

## R7. 앱 하나를 연 비용을 Electron 앱 하나 수준으로 (NFR-1, NFR-2)

소유자 목표: "안드로이드 앱 하나를 연 비용이 Electron 앱 하나를 연 비용과 비슷해야 한다." 이를 위해 프레임워크, system_server, zygote, ART 설정, init을 고쳐도 된다고 했다. 이 절은 공개 자료와 지금까지의 실측을 대조한 조사다. 절감량은 모두 추정치이고, 하드웨어에서 새로 측정하지 않았다.

### 지금의 실측 (Settings 하나가 열린 `Active`)

| 항목 | MiB |
| --- | --- |
| 호스트 합계 | 약 826 |
| VM 상주 (`vmmem`) | 670 |
| 창 관리자 (`dsa-wm`) | 119 |
| 앱 창 실행 파일 | 38 |
| 게스트 PSS: system_server | 131 |
| 게스트 PSS: Settings | 101 |
| 게스트 PSS: 에이전트 | 46 |
| 게스트 PSS: 나머지 시스템 프로세스 | 약 110 |
| 커널과 기타 | 336 |
| 페이지 캐시 | 276 |

프레임워크를 고치지 않을 때의 하한은 대기 약 350-400 MiB로 추정한다. 코덱 지연 적재, 창 관리자 GPU 해제, 커널 메모리 정리, Cuttlefish 전용 서비스 제거, 고정 RAM 상한 제거, 마지막 창을 닫으면 앱 종료는 다른 작업이 진행 중이라 여기서 다루지 않는다.

### 1. Electron 앱 하나의 비용

측정 방법을 밝힌 공개 수치는 적다. 찾은 것은 다음과 같다.

| 대상 | 대기 메모리 | 출처와 성격 |
| --- | --- | --- |
| 최소 Electron 앱 (Windows) | 약 120 MB | levminer.com, 2022년 작성자 실측 |
| 최소 Electron 앱 (Ubuntu) | USS 118 MB, PSS 207 MB | tauri-apps/tauri 이슈 #5889의 측정 |
| Chromium 기본 비용 | 120-200 MB | dev.to 글, 추정 |
| 메모 앱 수준 Electron 앱 | 약 281 MB | tech-insider.org 비교 글 |
| 일반 Electron 앱 범위 | 150-400 MB | pkgpulse.com 비교 글 |
| Discord | 백그라운드 50 MB부터, 사용 중 300-600 MB | itgeared.com |
| VS Code, Slack | 공개 실측을 찾지 못했다 | 확장과 워크스페이스에 따라 크게 달라진다 |

출처:
- https://www.levminer.com/blog/tauri-vs-electron
- https://github.com/tauri-apps/tauri/issues/5889
- https://dev.to/gerus_team/your-electron-app-is-a-lie-and-heres-the-memory-bill-483b
- https://tech-insider.org/tauri-vs-electron-2026/
- https://www.pkgpulse.com/guides/electron-vs-tauri-2026
- https://www.itgeared.com/how-much-ram-does-discord-use

정리: 최소 Electron 앱은 Windows에서 약 120-200 MB, 흔한 Electron 앱은 150-400 MB다. 그래서 "Electron 앱 하나 수준"을 두 숫자로 잡는다.

- 단기 목표: 단순한 앱 하나(Settings)를 연 `Active`에서 DSA 호스트 전체 메모리 400 MiB 이하. 흔한 Electron 앱 범위의 위쪽 끝이다.
- 최종 목표: 같은 조건에서 250 MiB 이하, 앱을 하나 더 열 때 늘어나는 몫 80 MiB 이하. 최소 Electron 앱과 같은 체급이다.

공개 수치의 질이 낮으므로, 같은 PC에서 최소 Electron 앱(고정 버전, 창 하나), VS Code, Discord를 띄워 10분 뒤 작업 관리자의 개인 작업 집합과 커밋 크기를 기록해 기준을 확정하기를 제안한다. `vmmem`은 게스트 메모리 전체가 개인 메모리이므로 비교는 개인 작업 집합끼리 한다.

### 2. 안드로이드의 기본 메모리는 어디로 가는가

| 구성 요소 | 일반적인 비용 | 줄이는 수단 |
| --- | --- | --- |
| 부트 이미지와 boot classpath (`boot.art`, `boot.oat`, 프레임워크 jar) | 파일 매핑 수십 MiB, 모든 Java 프로세스가 공유 | 미리 적재하는 클래스 축소 |
| zygote의 미리 적재한 클래스와 리소스 (`preloaded-classes`, 미리 적재한 drawable) | zygote64 PSS 수십 MiB, 자식과 공유 | 목록 축소. 앱 시작이 느려진다 |
| system_server | 100-200 MiB PSS. 서비스 약 100개, 각자 스레드와 Java 힙 | 서비스 끄기 (설정 또는 소스 수정) |
| ART 힙과 JIT 코드 캐시 | 프로세스마다 수 MiB에서 수십 MiB | `dalvik.vm.heap*`, JIT 캐시 상한, 프로파일 저장 끄기 |
| SurfaceFlinger | 수십 MiB, 그래픽 버퍼 별도 | DSA는 화면을 호스트가 그린다 |
| HAL 프로세스 (vendor) | 하나당 2-10 MiB | 쓰지 않는 HAL 제거 |
| Java 시스템 앱 (PermissionController, MediaProvider, ExtServices, NetworkStack 등) | 하나당 20-60 MiB PSS | 지연 시작, 상주 해제, 제거 |
| 커널 (slab, 페이지 테이블, binder, 드라이버) | 수십에서 수백 MiB | 다른 작업이 진행 중 |

자료:
- AOSP `build/make/target/product/go_defaults_common.mk`: `ro.config.low_ram=true`, system_server를 `speed-profile`로 컴파일, eng 빌드가 아니면 Scudo 끄기, NetworkStack을 system_server 안에서 돌리기. https://android.googlesource.com/platform/build/+/38afe9570b/target/product/go_defaults_common.mk
- Android (Go edition) 개발자 문서: 최소 RAM은 Android 8.1-10에서 512 MB, 11-12에서 1 GB, 13에서 2 GB. 저사양 설정은 PIP, 화면 분할, 다중 디스플레이, 다른 앱 위에 그리기를 끈다. https://developer.android.com/guide/topics/androidgo
- ChromeOS ARC++ on Demand: Android 앱이 없으면 Android를 시작하지 않아 약 750 MB를 아낀다고 Google이 밝혔다. ARCVM은 virtio-balloon과 MGLRU로 메모리를 호스트와 나눈다. https://chromeos.dev/en/posts/improving-performance-with-new-arc-resource-management-features
- WSA와 Waydroid의 대기 메모리는 측정 방법을 밝힌 공개 수치를 찾지 못했다. Anbox와 Waydroid는 호스트 커널을 함께 써서 커널 몫이 따로 없으므로 비교 대상으로 맞지 않는다.

해석: 512 MB 기기에서 돌던 Android 8.1 Go가 보여 주듯, 안드로이드 자체는 300 MiB 안쪽에서 돌 수 있다. 다만 Go 구성은 기기 제약 때문에 기능을 끈 것이고, 그중 일부(다중 창, 다른 앱 위에 그리기)는 DSA의 자유형 창과 WSA 수준 호환성에 필요하다(R4의 `low_ram` 결정).

### 3. 선택지와 순위

절감량은 위 실측(게스트 PSS 합 약 390 MiB, 커널과 캐시 약 610 MiB)을 기준으로 한 추정이다. 호환성은 WSA에서 돌던 앱이 그대로 도는지로 본다.

| 순위 | 선택지 | 추정 절감 | 앱 호환성 | 노력과 위험 |
| --- | --- | --- | --- | --- |
| 1 | (a) 설정만: Go 계열 속성 일부(`low_ram`은 끈 채), 힙과 JIT 상한, 프로파일 저장 끄기, USAP 끄기, 캐시 프로세스 수 축소, 오버레이로 서비스 끄기 | 60-150 MiB | 거의 영향 없음. 힙 상한은 큰 앱의 GC를 늘린다 | 낮음. 지금의 오버레이와 `build.prop` 편집으로 된다 |
| 2 | (a) Java 시스템 앱 정리: 상주가 필요 없는 앱을 지연 시작으로, 쓰지 않는 앱 제거 | 40-120 MiB | 사진과 파일 접근에는 MediaProvider가 필요하다. 제거 대상 고르기가 관건이다 | 낮음에서 중간 |
| 3 | (c) system_server와 프레임워크를 소스에서 빌드: 쓰지 않는 서비스(텔레포니, 프린트, 음성 상호작용, 백업 등)를 시작하지 않는 패치, 미리 적재 목록 축소 | 60-120 MiB | 서비스가 없어 `getSystemService`가 null이면 죽는 앱이 있다. 빈 서비스를 남겨야 한다 | 높음. 아래 비용 참조 |
| 4 | (b) 이미지 빌드에서 바이너리 편집: `services.jar`의 dex에서 서비스 시작 호출을 바꾸기 | 30-80 MiB | 3과 같다 | 중간. 메서드 크기를 지키는 편집만 되고, 바뀐 jar는 system_server odex와 맞지 않아 기기에서 다시 컴파일된다. 버전마다 편집점을 다시 찾아야 한다 |
| 5 | (d) 앱 여럿이 한 ART 프로세스를 나눠 쓰기 | 추가 앱 하나당 10-30 MiB | 낮다. UID와 SELinux 앱 격리가 깨진다 | 매우 높음. 권하지 않는다 |
| 참고 | 커널과 페이지 캐시 (다른 작업이 진행 중) | 200-400 MiB | 영향 없음 | 진행 중 |

(c)의 비용:
- 필요한 것은 `frameworks/base`, `art`, `frameworks/native`지만 Soong은 트리 전체를 요구한다. AOSP 체크아웃은 100 GB 이상, 빌드 출력을 더하면 300 GB 안팎이다.
- 첫 빌드는 64코어 서버에서 1-2시간이다. GitHub 기본 러너(4코어, 빈 디스크 약 14 GB)는 디스크 때문에 안 된다. 큰 러너(유료)나 자체 호스팅 러너가 필요하고, ccache와 `out/` 보존으로 증분 빌드를 줄인다.
- 바뀐 모듈만 꺼내 공개 `target_files`의 같은 파일을 바꾸면 D14의 공개 빌드 재료 구조를 지킬 수 있다. 단, 같은 빌드 번호의 소스 태그로 빌드해야 boot classpath와 서명이 맞는다.
- INTENT 영향: D14의 "AOSP 전체를 빌드하지 않는다"를 바꿔야 한다. D5는 바뀌지 않는다. 커널, ART, 프레임워크는 그대로 게스트에 남고, 그 안에서 덜 시작할 뿐이다.

### 4. 권고와 단계별 계획

1. 1단계 (설정만): 순위 1, 2와 진행 중인 커널, 창 관리자 작업. 목표: Settings 하나를 연 `Active`에서 호스트 합계 500 MiB 이하.
2. 2단계 (바이너리 편집 시험): system_server 서비스 시작 목록을 줄이는 편집을 한 버전에서 시험하고, CTS 일부와 인기 앱 목록으로 호환성을 확인한다. 목표: 400 MiB 이하(단기 목표).
3. 3단계 (소유자 결정 뒤, 소스 빌드를 CI로): 자체 호스팅 러너에서 `services.jar`, `framework.jar`, 부트 이미지를 빌드해 공개 재료에 갈아 끼우고, 미리 적재 목록을 데스크톱 앱 기준으로 다시 만든다. 목표: 250 MiB 이하, 추가 앱 하나당 80 MiB 이하(최종 목표).
4. (d)는 하지 않는다. 앱 격리를 깨므로 호환성과 보안을 지킬 수 없다.

### 5. 소유자에게 제안하는 문구 (SPEC과 INTENT는 바꾸지 않았다)

NFR 제안 (새 항목, 가칭 "앱 하나의 메모리"):
- 목표: Google 서비스 계열이 없는 구성에서, 단순한 앱 하나(Settings)를 연 `Active` 상태로 10분 뒤 DSA 호스트 프로세스 전체(게스트 VM, 동반 서비스, 창 관리자, 앱 창)의 개인 작업 집합 합계가 400 MiB 이하다. 최종 목표는 250 MiB이고, 앱을 하나 더 열 때 늘어나는 몫은 80 MiB 이하다.
- 측정 방법: 같은 PC에서 최소 Electron 앱(고정 버전, 창 하나)의 같은 값을 함께 기록하고, 그 값과의 비율도 적는다.
- 비용: 앱 하나 기준 상한이 생겨 기능을 더할 때마다 메모리 회귀를 막아야 한다. 얻는 것: 소유자의 목표가 검증 가능한 숫자가 된다.

INTENT 제안:
- D14에 "선택한 프레임워크 모듈(`services.jar`, `framework.jar`, 부트 이미지)은 같은 빌드 번호의 소스 태그에서 DSA 패치로 빌드해 공개 빌드 재료의 해당 파일을 바꾼다"를 더한다. 비용: 자체 호스팅이나 큰 러너, 디스크 300 GB, 버전마다 패치 유지. 얻는 것: 설정만으로 닿지 않는 약 100 MiB.
- D8의 추정치(대기 500-800MB)를 위 목표로 바꾼다.
- D5는 그대로 둔다.

### 확인하지 못한 것

- Electron 기준값의 이 PC 실측.
- 위 절감량 전부. system_server의 서비스별 비용은 `dumpsys meminfo`로 나뉘지 않으므로, 서비스를 하나씩 끈 이미지로 차이를 재야 한다.
- WSA, Waydroid의 대기 메모리 공개 실측.

## R9. ARM 앱 번역 계층: 안드로이드의 네이티브 브리지와 오픈 소스 후보 (PROJECT Q3)

**기준.** 2026-11-05, 공개 자료와 소스 저장소를 읽었다. 이 조사에서 빌드하거나 실행하지 않았다. 게스트는 x86_64 전용이고 32비트 zygote가 없다.

### 안드로이드의 네이티브 브리지

- zygote는 `ro.dalvik.vm.native.bridge`가 비어 있지 않고 `0`이 아니면 그 이름의 라이브러리를 브리지로 쓴다(ART의 `-XX:NativeBridge`). 이름은 경로가 아니라 파일 이름이다(`NativeBridgeNameAcceptable`이 검사한다). 라이브러리는 기본 링커 이름 공간(`/system/lib64` 등)에서 찾는다. DSA는 영숫자, `.`, `_`, `-`로 된 `.so` 이름만 받는다.
- 브리지는 `NativeBridgeItf` 심볼로 `NativeBridgeCallbacks` 표를 내보낸다. AOSP `art/libnativebridge/include/nativebridge/native_bridge.h`(main)의 주석으로 보면, 버전이 오를 때마다 함수가 붙었다: 호환성 확인과 신호 처리(v2), 링커 이름 공간과 `unloadLibrary`(v3), 내보낸 이름 공간(v5, Android 10), `getTrampoline`을 대신하는 `getTrampolineWithJNICallType`(v7), `isNativeBridgeFunctionPointer`(v8). v4, v6과 `preZygoteFork`, `getTrampolineForFunctionPointer`가 붙은 버전은 주석에 없다. Android 16과 17이 받는 최고 버전은 그 가지의 헤더로 확인해야 한다. 브리지는 자기가 만든 버전을 알리고, 런타임은 그보다 새 함수를 부르지 않는다.
- 앱이 ARM 라이브러리만 담고 있으면, 패키지 관리자는 `ro.product.cpu.abilist`(앞이 우선)에 있는 ABI만 고른다. 목록에 `arm64-v8a`가 없으면 설치가 `INSTALL_FAILED_NO_MATCHING_ABIS`로 실패한다. 목록은 init이 부팅 때 파티션별 `ro.<파티션>.product.cpu.abilist{,32,64}`에서 만들고, 그 뒤에는 바꿀 수 없다(`ro.*`).
- 앱 프로세스는 `ro.dalvik.vm.isa.arm64=x86_64`로 ARM64 명령 집합이 x86_64 위에서 번역된다는 것을 안다.
- ARM 실행 파일(앱이 `exec`하는 것)은 `ro.enable.native.bridge.exec=1`과 binfmt_misc 처리기(`/proc/sys/fs/binfmt_misc/register`에 ELF 머리와 번역기 실행 파일을 등록)로 돈다.
- 번역기는 ARM64 게스트 라이브러리(`libc.so`, `libm.so` 같은 bionic, `/system/lib64/arm64/`)와 그 링커 구성, 그리고 호스트 라이브러리로 넘기는 프록시 라이브러리를 함께 필요로 한다.
- 32비트 ARM(`armeabi-v7a`)은 32비트 zygote(`app_process32`)가 있어야 한다. DSA 게스트는 64비트 전용이므로 `armeabi-v7a` 전용 앱은 번역 계층이 있어도 돌지 않는다.

### 다른 제품이 하는 일 (설명만, 코드는 가져오지 않는다)

| 제품 | 방식 | 라이선스 |
|---|---|---|
| WSA | Intel Bridge Technology(Houdini 계열). 이미지에 들어 있었다 | 독점 |
| ChromeOS ARCVM | Intel PC는 Houdini, AMD PC는 Google의 libndk_translation | 독점 |
| BlueStacks | 자체 번역기를 이미지에 넣는다 | 독점 |
| Android Emulator(Google APIs x86_64, Android 14-17) | `ro.dalvik.vm.native.bridge=libndk_translation.so`. Berberis를 바탕으로 한 닫힌 ARM64 백엔드 | 독점 |
| Waydroid | 이미지에는 없다. 별도 스크립트(GPL-3.0)가 사용자 PC에서 Houdini나 libndk_translation을 받아 시스템 오버레이에 넣고 속성을 고친다 | 번역기는 독점 |
| Anbox(옛 오픈 소스판) | ARM 번역 없음 | |

공통점은 둘이다. 번역기가 모두 독점이고, 속성과 라이브러리를 부팅 전에 이미지(또는 오버레이)에 넣는다. Waydroid의 방식, 곧 사용자가 가져온 번역기를 시스템 위에 얹는 구조가 DSA의 플러그인 구조와 가장 가깝다.

### 오픈 소스 후보

| 후보 | 방향 | 라이선스 | Apache-2.0 옆 배포 | 안드로이드 앱 | 상태 |
|---|---|---|---|---|---|
| Berberis(AOSP `platform/frameworks/libs/binary_translation`) | riscv64 → x86_64 | Apache-2.0 | 된다 | riscv64 앱만. 저장소에 ARM 게스트가 없다 | Google이 유지. README는 CTS와 일부 앱이 돈다고 한다 |
| Digitalis(`github.com/DigitalisX64/digitalis`) | arm64 → x86_64 | Apache-2.0(Berberis 수정판) | 된다 | ARM64 앱 | 활발함(마지막 푸시 2026-10-30, 별 20개). `android16`과 `android17` 가지. 배포판 없음. 자체 보고: 해석기, 가벼운 JIT, 최적화 JIT, 프록시 라이브러리 21개, 호스트 테스트 3600개 이상, 예제 앱 159개 |
| FEX-Emu | x86/x86_64 → arm64 | MIT | 된다 | 방향이 반대다 | |
| box64, box86 | x86 → arm64, riscv64 | MIT | 된다 | 방향이 반대다 | |
| QEMU 사용자 모드 | arm64 → x86_64 | GPL-2.0 | 별도 실행 파일로는 되지만 브리지 라이브러리에 넣으면 GPL이 된다 | 프로세스 전체를 번역한다. 네이티브 브리지가 아니므로 ART 전체를 ARM으로 돌려야 하고, 느리고 ARM 시스템 이미지가 따로 필요하다 | |
| Darling | macOS 바이너리 → Linux | GPL-3.0 | | 명령 집합 번역이 아니다 | |
| libhoudini, libndk_translation | arm → x86 | 독점 | 안 된다 | | 배포 금지 |

Digitalis의 배포 묶음(`docker/build-digitalis.sh`)은 번역기(`libberberis_arm64.so`), `libberberis_exec_region.so`, 프록시 라이브러리, `berberis_program_runner_arm64`와 binfmt_misc 처리기, ARM64 게스트 라이브러리(`system/lib64/arm64/`), `app_process64`와 `linker64`의 ARM64판, `ld.config.arm64.txt`, binfmt_misc 규칙을 담는다. 속성은 `ro.dalvik.vm.native.bridge=libberberis_arm64.so`, `ro.dalvik.vm.isa.arm64=x86_64`, `ro.enable.native.bridge.exec=1`이다.

### 결정

1. **플러그인 구조를 먼저 둔다.** 번역기는 이미지와 별도인 묶음 하나(읽기 전용 erofs 이미지와 그 설명 파일)로 받는다. 게스트는 그 묶음이 사용자 디스크에 있고 설정이 켜져 있을 때만 부팅 때 속성을 정하고 파일을 시스템 위에 얹는다. 묶음이 없으면 이미지는 지금과 같다(`x86_64`만 알린다).
2. **기본 번역기로 Digitalis를 후보로 둔다.** 라이선스는 Apache-2.0이라 함께 배포할 수 있다. 다만 지금 기본으로 넣지는 못한다. 이유는 셋이다.
   - 빌드 비용: Berberis는 Soong 안에서만 빌드되고, Digitalis의 묶음은 AOSP 트리 전체(`repo sync` 수백 GB, 이미지 조립까지 한 `out/`)에서 나온다. GitHub 호스트 러너(디스크 약 14 GB, 작업 6시간)로는 못 한다. 큰 자체 호스트 러너가 있거나 Digitalis가 배포판을 내야 한다.
   - 버전: DSA 게스트는 Android 17이다. Digitalis는 `android17` 가지가 있지만 배포 묶음 문서는 Android 16 트리를 기준으로 한다. 번역기와 게스트 라이브러리는 같은 플랫폼에서 나와야 한다.
   - 검증: 호환성 수치는 자체 보고뿐이다. DSA에서 ARM64 전용 앱으로 실측해야 한다.
3. 저장소는 Digitalis를 받아 묶음을 만드는 스크립트(`dsa/guest/arm-translation/`)를 둔다. 받는 것은 `.scratch/` 아래로만 받고, 코드를 들여오지 않는다. 큰 PC에서 그 스크립트로 만든 묶음을 `dsa arm-translation install`로 넣어 시험할 수 있다.

### 대가

- 번역기를 시스템 위에 얹으려면 게스트 커널에 overlayfs와 loop 장치, binfmt_misc가 있어야 한다.
- 묶음은 사용자 디스크(`/data/vendor`)에 들어가므로 사용자마다 한 벌이다(수십에서 100여 MB).
- `ro.product.cpu.abilist`는 부팅 때만 정해지므로, 묶음을 넣거나 빼면 안드로이드를 다시 시작해야 한다.

### 확인하지 못한 것

- Digitalis 묶음이 Android 17 DSA 게스트에서 실제로 돌고, 어떤 앱이 도는지.
- 번역된 앱의 성능(시작 시간, 프레임 시간). 공개된 수치가 없다.
- 오버레이 위의 라이브러리를 앱 프로세스가 SELinux 거부 없이 읽는지. PC에서 `dmesg`로 확인한다.

## R10. ARM64 Windows 호스트: WSL의 arm64 부팅과 Hyper-V ARM64의 차이 (FR-27)

**기준.** 2026-11-05. WSL 소스 `microsoft/WSL` `master`(커밋 `8a5e541`)의 `src/windows/service/exe/WslCoreVm.cpp`, `src/windows/common/hcs_schema.h`, `src/windows/common/helpers.cpp`와 커널 저장소 `microsoft/WSL2-Linux-Kernel` 태그 `linux-msft-wsl-6.18.54.1`의 `Microsoft/config-wsl-arm64`, DSA 커널 패치 다섯 개를 읽었다. ARM64 PC에서 부팅해 보지는 않았다.

### WSL은 arm64 커널을 어떻게 부팅하는가

- **직접 부팅이 없다.** WSL은 x86_64에서 `Chipset.LinuxKernelDirect`(`KernelFilePath`, `InitRdPath`, `KernelCmdLine`)를 쓰고, ARM64에서는 주석대로 "Linux kernel direct boot is not yet supported on ARM64"라서 `Chipset.Uefi.BootThis`를 쓴다. 항목은 `DeviceType: "VmbFs"`, `VmbFsRootPath`(폴더), `DevicePath`(`\` + 커널 파일 이름), `OptionalData`(커널 명령줄) 넷이다. UEFI 펌웨어가 VMBus 파일 시스템으로 폴더를 읽어 커널의 EFI 스텁을 시작하고, 스텁은 `OptionalData`를 명령줄로 받는다.
- **initrd는 명령줄로 넘긴다.** WSL의 명령줄은 `initrd=\initrd.img`로 시작한다. EFI 스텁이 자기가 시작된 폴더에서 그 파일을 읽는다. 그래서 커널과 initrd가 한 폴더에 있어야 하고, WSL은 ARM64에서 둘을 임시 폴더에 복사한 뒤 그 폴더를 `VmbFsRootPath`로 준다.
- **콘솔.** x86_64는 `earlycon=uart8250,io,0x3f8,115200`과 `console=ttyS0,115200`, ARM64는 `earlycon=pl011,0xeffec000,115200`과 `console=ttyAMA0`이다. 두 아키텍처 모두 `Devices.ComPorts["0"]`에 named pipe를 연결한다. 디버거 포트는 x86_64가 `ttyS1`, ARM64가 `ttyAMA1`이다.
- **공통 매개변수.** `nr_cpus`, `hv_utils.timesync_implicit=1`, `printk.devkmsg=on`, `page_reporting.page_reporting_order`는 아키텍처와 상관없이 붙는다. 메모리 설정(`AllowOvercommit`, `EnableDeferredCommit`, cold hint, `HostingProcessNameSuffix`)도 같다. 성능 카운터(`EnablePerfmonPmu`)만 x86_64 전용이다. 중첩 가상화는 ARM64에서 지원되지 않을 수 있다고 WSL이 적어 둔다.

### Hyper-V ARM64가 커널에 요구하는 것

- `config-wsl-arm64`에는 `CONFIG_EFI=y`, `CONFIG_EFI_STUB=y`, `CONFIG_ACPI=y`(기계 설명은 장치 트리가 아니라 ACPI), `CONFIG_SERIAL_AMBA_PL011=y`와 그 콘솔, `CONFIG_HYPERV=y`, `CONFIG_HYPERV_VMBUS=y`, `CONFIG_HYPERV_TIMER=y`, `CONFIG_HYPERV_UTILS=y`, `CONFIG_HYPERV_STORAGE=y`, `CONFIG_HYPERV_NET=y`, `CONFIG_HYPERV_BALLOON=y`, `CONFIG_PAGE_REPORTING=y`, `CONFIG_HYPERV_VSOCKETS=y`, `CONFIG_ARM64_4K_PAGES=y`가 모두 있다. `CONFIG_ANDROID_BINDER_IPC`는 꺼져 있고 `CONFIG_PREEMPT`도 꺼져 있어, `android.config`가 x86_64와 같이 켠다.
- ARM64의 balloon 드라이버는 빈 페이지 보고만 하고 메모리 hot-add는 하지 않는다. DSA가 메모리를 돌려받는 길은 빈 페이지 보고이므로 잃는 것이 없다.
- 커널 이미지는 `bzImage`가 아니라 `arch/arm64/boot/Image`다. EFI 스텁이 들어 있으면 PE 이미지(`MZ`)이고 오프셋 56에 arm64 Image 표식 `ARM\x64`가 있다. CI가 이 둘을 확인한다.

### DSA 패치는 arm64에서 맞는가

| 패치 | 건드리는 곳 | arm64 |
|---|---|---|
| `vmbus-platform-device` | `drivers/hv/vmbus_drv.c`의 `vmbus_acpi_add` | 맞다. Hyper-V는 ARM64에서도 VMBus를 ACPI로 알리므로 같은 함수를 지난다. 바로 아래 ARM64 `_CCA` 처리 코드는 그대로 둔다 |
| `cmdline-partition-uuid` | `block/partitions/cmdline.c` | 맞다. 아키텍처와 무관하다 |
| `memfd-ashmem-shim` | `mm/` | 맞다. `CONFIG_COMPAT` 분기는 32비트 앱용이고 arm64에서도 그대로 컴파일된다 |
| `cgroup-v1-sys-nice` | `kernel/cgroup/cgroup-v1.c` | 맞다 |
| `wakeup-sources-virtual` | `drivers/base/power/wakeup_stats.c` | 맞다. ARM64의 VMBus 장치 경로도 호스트마다 바뀐다 |

`arch/x86` 아래를 건드리는 패치는 없고, 테스트가 이를 지킨다.

**x86 전용 가정 하나는 호스트 명령줄에 있었다.** ramoops 영역을 `memmap=1M$0x10000000`으로 잡는데, `memmap=`은 x86 매개변수이고 ARM64 VM의 메모리 배치는 펌웨어가 정하므로 256 MiB가 RAM이라는 보장이 없다. ARM64에서는 `reserve_mem=1M:4096:dsa_ramoops ramoops.mem_name=dsa_ramoops`로 이름으로 잡는다(커널 6.12부터). 나누는 크기는 x86_64와 같다.

### 결정

1. 커널은 같은 태그에서 `config-wsl-arm64` + `android.config` + `android-arm64.config`로 만든다. `android-arm64.config`는 위 부팅, VMBus, 빈 페이지 보고에 필요한 옵션을 이름으로 적어, 새 WSL 태그가 하나라도 빼면 빌드가 실패한다. CI는 `ubuntu-24.04-arm` 러너에서 직접 빌드하고, 다른 기계에서는 `aarch64-linux-gnu-`로 교차 컴파일한다. 산출물 이름은 `kernel-arm64`다.
2. 호스트는 빌드된 아키텍처(`target_arch`)로 게스트 아키텍처를 정한다. Hyper-V는 같은 아키텍처의 게스트만 돌린다. ARM64에서는 WSL처럼 `Uefi.BootThis`(`VmbFs`)로 부팅하고, 콘솔은 PL011, ramoops는 `reserve_mem=`이다. 나머지 문서(메모리, SCSI, hvsocket, `ComPorts`)는 같다.
3. **부팅마다 폴더 하나.** VM은 `VmbFsRootPath` 폴더 전체를 읽을 수 있다. 기계의 `tmp` 폴더에는 다른 사용자의 부팅 initrd(비밀이 든)가 있을 수 있으므로, 그 폴더를 그대로 주지 않는다. 부팅마다 `tmp\<고유 이름>\`을 만들어 커널 사본과 `initrd.img`만 넣고, VM 시작이 끝나면 지운다.

### 대가

- ARM64에서는 부팅마다 커널(수십 MB)을 복사한다. WSL도 같은 복사를 한다.
- ARM64 게스트는 arm64 전용 이미지가 따로 있어야 한다(이미지 작업은 별도).

### 확인하지 못한 것

- 실제 ARM64 PC(Snapdragon)에서 이 문서로 VM이 만들어지고 부팅하는지, 그리고 `HcsGrantVmAccess`가 폴더에 대해 VmbFs 읽기를 허락하는지.
- PL011 주소 `0xeffec000`이 모든 ARM64 Windows 빌드에서 같은지. WSL이 고정값으로 쓰므로 같다고 본다.
- `reserve_mem=`으로 잡은 ramoops가 Hyper-V ARM64에서 패닉 기록을 남기는지.
- ARM64에서 `EnableColdHint`와 fine-grained memory 설정이 받아들여지는지. WSL은 아키텍처를 가리지 않고 설정한다.
## R11. 디스크를 SCSI 탐색 순서가 아니라 고정된 정체로 찾기 (FR-1)

질문: 게스트 이미지는 시스템 디스크가 `sda`라고 가정했다(`blkdevparts=sda:...`). 호스트가 pstore 디스크처럼 디스크를 하나 더 붙이면 Hyper-V SCSI 탐색 순서에 따라 이름이 바뀌어 다른 디스크로 부팅하거나 부팅에 실패할 수 있는가. 그렇다면 어떻게 고치는가.

### 확인한 자료

| 자료 | 버전 |
|---|---|
| WSL 커널 `block/partitions/cmdline.c`, `check.h`, `efi.c`, `core.c` | `linux-msft-wsl-6.18.54.1` |
| AOSP `system/core/init` (`devices.cpp`, `block_dev_initializer.cpp`), `fs_mgr` | Android 16 / 17 |
| 이 저장소의 `make-image.py`, `fstab.dsa`, `*.rc`, `vm.rs` | `develop` `cf3e880` |

### 방법

`/dev/block/sd`, `/dev/sd`, `blkdevparts=` 를 저장소 전체에서 찾았고, 커널 명령줄 파티션 파서가 디스크를 고르는 코드를 읽었다. 하드웨어에서는 아직 확인하지 않았다.

### 찾은 것

1. `/dev/block/sdX`를 직접 쓰는 곳은 없었다. `fstab.dsa`의 `/metadata`, `/data`, `misc`와 부트 컨트롤 HAL이 쓰는 `misc`, 첫 단계 마운트가 쓰는 `vbmeta`, `boot`, `super`는 모두 `/dev/block/by-name/` 링크로 찾는다. 이 링크는 파티션 이름(uevent `PARTNAME`)으로 만들어지므로 디스크 순서와 무관하다.
2. 순서에 기대는 곳은 하나였다. 커널 명령줄의 `blkdevparts=sda:...`다. `cmdline_partition()`은 `state->disk->disk_name`, 곧 탐색 순서로 붙은 커널 이름으로 항목을 찾는다. pstore 디스크가 먼저 탐색되면 시스템 디스크의 파티션 범위와 이름이 pstore 디스크에 붙고, 시스템 디스크는 GPT 파서로 넘어간다. 이름이 엉뚱한 디스크에 붙으면 `by-name/super`가 다른 디스크를 가리킬 수 있다.
3. 호스트는 SCSI 위치를 고정한다(0번 시스템, 1번 데이터). 그러나 리눅스 storvsc가 디스크에 `sd` 이름을 주는 순서는 비동기 탐색이 끝나는 순서이고, 위치 번호를 따른다는 보장이 없다. 그래서 위치 고정만으로는 충분하지 않다.

### 결정

- 시스템 디스크의 GPT에 고정된 디스크 GUID(`SYSTEM_DISK_GUID`)를 `sgdisk --disk-guid`로 쓰고, 커널 명령줄은 `blkdevparts=<그 GUID>:...`로 디스크를 그 GUID로 이름 짓는다.
- 커널 패치(`cmdline-partition-uuid.patch`)를 넓힌다. 커널 이름으로 맞는 항목이 없으면 디스크의 GPT 헤더(LBA 1, 논리 블록 크기 기준)를 읽어 서명 `EFI PART`를 확인하고, 오프셋 56의 디스크 GUID를 `%pUl` 형식으로 바꿔 항목을 찾는다. 항목 이름 버퍼는 GUID가 들어가도록 37바이트로 늘린다. 기존 `sda:` 형식도 그대로 동작한다.
- 데이터 디스크(`misc`, `metadata`, `userdata`)는 지금처럼 자기 GPT의 파티션 이름으로 찾는다. 사용자마다 템플릿을 복사하므로 디스크 GUID가 같아도 상관없다. 시스템 디스크 GUID와만 다르면 된다.
- `check-image.py`는 커널 명령줄, 이미지 안의 `fstab.*`, `*.rc`, `*.sh`, `*.prop`, 램디스크의 마운트 표와 init 스크립트가 `/dev/block/sdX`, `/dev/sdX`, `blkdevparts=sdX:`를 쓰면 실패한다.

### 남은 위험과 확인할 것

- `androidboot.boot_devices=vmbus`는 VMBus 아래 모든 디스크의 파티션에 `by-name` 링크를 만든다. 다른 디스크가 `misc`, `super`, `userdata` 같은 같은 이름의 GPT 파티션을 가지면 링크가 충돌한다. pstore 디스크는 파티션 표가 없는 원시 디스크여야 한다.
- 하드웨어 확인(예정): pstore 디스크를 SCSI 0번, 1번, 2번에 각각 붙여 부팅하고, 콘솔 로그의 파티션 줄(`sdX: (vbmeta_a)(...)(super)`)이 시스템 디스크에 붙는지, `ls -l /dev/block/by-name/`이 같은 디스크를 가리키는지, `/data`가 마운트되는지 본다. 결과는 이 절에 기록한다.

## R14. Google의 ARM 번역기: SDK 저장소 목록과 에뮬레이터 이미지의 구조 (FR-25)

**기준.** 2026-11-07. Google SDK 저장소의 시스템 이미지 목록 `https://dl.google.com/android/repository/sys-img/google_apis/sys-img2-1.xml`부터 `sys-img2-5.xml`까지를 받아 읽었다(`sys-img2-6.xml`은 없다). Google APIs x86_64 이미지 두 개, `x86_64-36_r07.zip`(API 36, 1,895,447,397바이트, SHA-1 `c6bf44bdcd885bb902b4ba752d111a073ad7a817`)과 `x86_64-37.0_r08.zip`(API 37.0, 2,143,899,425바이트, SHA-1 `a266cfbcf4d6c769b166c0af4d68eff0e4f4a5e3`)을 `.scratch/google-ndk/`에 받아 목록의 해시와 맞는지 확인하고, 그 안의 `system.img`를 Python 스크립트(`.scratch/ndk-probe-*.py`)로 GPT, liblp 메타데이터, ext4 순서로 읽었다. 받은 파일은 저장소에 넣지 않았다. 게스트에 넣어 부팅해 보지는 않았다.

### 저장소 목록

- 목록은 스키마 버전마다 따로 있다(`sys-img2-1`부터 `-5`). API 36은 1부터, API 36.1과 37.0은 3부터 나온다. 같은 이미지를 가리키므로 가장 새 스키마부터 읽는다.
- 이미지 하나는 `<remotePackage path="system-images;android-36;google_apis;x86_64">`이다. `<type-details>` 안에 `<api-level>`(`36`, `36.1`, `37.0`, 확장판은 `36x`), `<extension-level>`, `<base-extension>`(확장판은 `false`), `<tag><id>`(`google_apis`, `google_apis_playstore`, `google_apis_ps16k` 등), `<abi>`와 `<abis>`, `<translatedAbis>arm64-v8a</translatedAbis>`가 있다. 그 밖에 `<revision><major>`, `<uses-license ref="android-sdk-license"/>`, `<channelRef ref="channel-0"/>`(0이 안정판, 나머지는 베타와 카나리), `<archives><archive><complete>`의 `<size>`, `<checksum type="sha1">`, `<url>`(목록과 같은 폴더의 파일 이름)이 있다.
- 해시는 SHA-1뿐이다. 크기와 함께 맞춰 본다. 목록은 HTTPS로 Google에서 받는다.
- x86_64 Google APIs 이미지의 약관은 `android-sdk-license`(Android SDK License Agreement)다. arm64 이미지는 `android-sdk-arm-dbt-license`라는 다른 약관을 쓴다. DSA는 x86_64 이미지만 받는다.
- 2026-11-07의 안정판은 API 36 r7, 36.1 r4, 37.0 r8이다. 36-ext18과 36-ext19는 확장판이다.

### 이미지의 구조

- zip 안에 `x86_64/system.img`(API 36은 4,415,553,536바이트, 37.0은 5,498,732,544바이트, deflate로 압축)가 있다. 4 GiB를 넘으므로 zip64다. `source.properties`의 `Pkg.Revision`은 목록의 리비전과 다를 수 있다(37.0 r8의 파일은 7이라고 쓴다). 그래서 버전은 목록에서 정한다.
- `system.img`는 디스크 전체다. GPT에 `vbmeta`(1 MiB부터)와 `super`(2 MiB부터) 두 파티션이 있다.
- `super`는 liblp 메타데이터(4096바이트 뒤의 기하 정보, 그 뒤 8192바이트에서 시작하는 메타데이터, 버전 10.0, 헤더 128바이트, 표의 SHA-256 확인)로 `system`, `system_dlkm`, `system_ext`, `product`, `vendor`를 담는다. `system`은 `super`의 1 MiB부터 선형 구간 하나이고, API 36은 1,049,964,544바이트, 37.0은 1,254,883,328바이트다.
- `system`은 ext4(블록 4 KiB, 64비트 기능 없음, 익스텐트, 공유 블록)다. 루트 아래 `system/` 폴더가 있는 system-as-root 구조다.
- 번역기 속성은 `system/build.prop`에 있다: `ro.dalvik.vm.native.bridge=libndk_translation.so`, `ro.dalvik.vm.isa.arm64=x86_64`, `ro.enable.native.bridge.exec=1`, `ro.system.product.cpu.abilist=x86_64,arm64-v8a`. 번역기 버전을 따로 적은 속성은 없다. 그래서 묶음 버전은 `ro.system.build.id`(36은 `BE2A.250530.026.F3`, 37.0은 `CE2A.260420.051.A3`), API 수준, 리비전으로 만든다.

### 번역기 파일

| 경로(`system/` 아래) | API 36 | API 37.0 |
|---|---|---|
| `lib64/libndk_translation.so` | 5,403,704바이트 | 8,743,696바이트 |
| `lib64/libndk_translation_proxy_*.so` | 21개(libEGL, libGLESv1_CM, libGLESv2, libGLESv3, libOpenMAXAL, libOpenSLES, libaaudio, libamidi, libandroid, libandroid_runtime, libbinder_ndk, libc, libcamera2ndk, libjnigraphics, libmediandk, libnativehelper, libnativewindow, libneuralnetworks, libvulkan, libwebviewchromium_plat_support) | 없음(번역기 안에 들었다) |
| `lib64/arm64/` | arm64 라이브러리 56개(bionic, libc++, libEGL과 GLES, libvulkan, ICU, libandroid, libmediandk, RenderScript 등) | 같은 56개 |
| `bin/arm64/` | `app_process64`, `linker64` | 같음 |
| `bin/ndk_translation_program_runner_binfmt_misc_arm64` | 있음 | 있음 |
| `etc/binfmt_misc/arm64_exe`, `arm64_dyn` | 있음 | 있음 |
| `etc/ld.config.arm64.txt` | 있음 | 있음 |
| `etc/cpuinfo.arm64.txt` | 있음 | 없음 |
| `etc/berberis/cpuinfo.arm64.txt` | 없음 | 있음 |

- 두 binfmt_misc 규칙은 `/system/bin/ndk_translation_program_runner_binfmt_misc_arm64`를 실행한다. DSA의 init 스크립트가 묶음의 규칙을 등록하므로 이미지의 `etc/init/ndk_translation.rc`는 필요 없다. 그 스크립트는 `/system/etc`를 덮기 전에 읽히므로 넣어도 쓰이지 않는다.
- 32비트 파일(`etc/binfmt_misc/arm_exe`, `arm_dyn`, `ld.config.arm.txt`, `cpuinfo.arm.txt`)도 있지만 32비트 라이브러리(`lib/arm`)는 없다. DSA 게스트에 32비트 zygote가 없으므로 넣지 않는다.
- `libndk_translation.so`가 링크하는 x86_64 라이브러리(`liblog`, `libandroid`(36만), `libdl_android`, `libc++`, `libc`, `libm`, `libdl`)와 프록시가 링크하는 것(`libEGL`, `libGLESv2`, `libvulkan`, `libmediandk` 등)은 모두 AOSP 시스템 라이브러리다. arm64 라이브러리가 서로 링크하는 것도 `lib64/arm64/` 안에 있다.
- 꺼내는 파일은 API 36이 84개 30,536,301바이트, 37.0이 64개 32,242,207바이트다. 묶음 이미지는 압축하지 않은 erofs라 그보다 조금 크다(블록 단위로 놓인다).

### 결정

1. 호스트(명령줄)가 목록을 읽고 게스트와 같은 API 수준(Android 16은 36, 17은 37.0)의 안정판, 기본판(`base-extension`이 `false`가 아님), `google_apis` x86_64, `translatedAbis`에 `arm64-v8a`가 있는 이미지를 고른다. 여럿이면 부 버전이 가장 낮은 것(36보다 36.1을 뒤로), 그다음 리비전이 가장 높은 것이다. 목록의 `<url>`은 같은 폴더의 파일 이름이거나 `https://dl.google.com/` 주소일 때만 받는다.
2. 받은 zip은 크기와 SHA-1을 목록과 맞춘다. 다르면 거부하고 아무것도 바꾸지 않는다.
3. zip을 풀지 않고 `system.img`를 스트리밍으로 inflate하면서 GPT와 liblp를 읽고, `system` 구간만 임시 파일로 쓴다(디스크 전체를 쓰지 않는다). 그 ext4에서 위 표의 파일만 꺼낸다. 이 일은 Rust로 직접 한다(inflate, SHA-1, GPT, liblp, ext4 읽기, erofs 쓰기). 세 호스트 모두 같은 코드이고, `mkfs.erofs`나 다른 도구를 새로 쓰지 않는다. 다운로드는 각 호스트가 이미 쓰는 것(Windows는 WinHTTP, macOS와 Linux는 curl)으로 한다.
4. 꺼낸 파일은 FR-25 묶음 형식 그대로다(`arm-translation.json`의 이름 `Google ARM translator`, 브리지 `libndk_translation.so`, ABI `arm64-v8a`, 라이선스 `Android SDK License`). 그래서 설치, 검사, 부팅 값, 오버레이는 사용자 묶음과 같은 길을 지난다. 받은 이미지와 임시 파일은 성공해도 실패해도 지운다.

### 대가

- 한 번에 약 2 GB를 받고, 그동안 zip(약 2 GB)과 `system` 구간(약 1 GB)을 임시 폴더에 둔다.
- Google이 목록 형식이나 이미지 구조(예: `system`을 erofs로 바꾸기)를 바꾸면 읽지 못하고 거부한다. 그때는 이 절을 다시 조사한다.

### 확인하지 못한 것

- 이 번역기를 DSA 게스트(AOSP Cuttlefish 빌드)에 얹었을 때 `arm64-v8a` 전용 앱이 도는지. 에뮬레이터 빌드와 Cuttlefish 빌드의 시스템 라이브러리가 같은 AOSP 가지에서 나와도 빌드가 다르다. 실제 PC에서 확인한다(FR-25 수동 기준).
- 36.1 이미지가 36 게스트에 더 맞는지. 지금은 기본판(36)을 고른다.

## R15. Google Play 설치 도구: LiteGapps Core의 구조와, 다시 빌드하지 않고 얹을 때 권한이 서는 조건 (FR-32)

**기준.** 2026-11-07. SourceForge의 LiteGapps 폴더 네 곳(`litegapps/x86_64/36/core`, `x86_64/37/core`, `arm64/36/core`, `arm64/37/core`)의 RSS 목록을 받아 읽었다. `LiteGapps-core-x86_64-16.0-20261001-official.zip`(122,196,440바이트, MD5 `ea9945a6d63849fa9e51782edfe613a4`)과 `LiteGapps-core-arm64-17.0-20261001-official.zip`(MD5 `52d7a470ad47be3dce1504e03a149f8e`)을 `.scratch/`에 받아 `unzip`, `xz -lvv`, `tar -t`로 풀어 보았다. 받은 파일의 MD5는 목록 값과 같았다. AOSP `android16-release`의 `core/java/android/content/pm/PackagePartitions.java`, `core/java/android/os/Build.java`, `services/core/java/com/android/server/pm/Settings.java`, `services/core/java/com/android/server/pm/permission/PermissionManagerServiceImpl.java`, `services/core/java/com/android/server/SystemConfig.java`를 읽었다. 게스트에서 설치해 보지는 않았다.

### 배포처와 목록

- 빌드 하나는 `litegapps/<arch>/<api>/core/<yyyy-mm-dd>/LiteGapps-core-<arch>-<android>-<yyyymmdd>-official.zip`에 있다. `arch`는 `x86_64`, `arm64`(그 밖에 `arm`, `x86`)이고, API 36과 37이 모두 있다. x86_64에는 `core`와 `lite`만 있고, arm64에는 `basic`, `go`, `micro`, `nano`, `pixel`, `user` 등도 있다.
- `https://sourceforge.net/projects/litegapps/rss?path=/litegapps/<arch>/<api>/core`가 그 폴더의 파일을 최신 순으로 준다. 항목마다 경로(`<title>`), 크기(`filesize`), MD5(`<media:hash algo="md5">`)가 있다. 공개된 체크섬은 MD5뿐이다.
- 받는 주소는 `https://sourceforge.net/projects/litegapps/files/litegapps/<arch>/<api>/core/<날짜>/<파일>/download`이고, HTTPS 미러로 넘어간다. curl `-L`과 WinHTTP가 그대로 따라간다.
- 2026-11-07의 최신은 네 곳 모두 20261001이다. 크기는 x86_64/36 122 MB, x86_64/37 149 MB, arm64/36 215 MB, arm64/37 257 MB이다.

### zip의 구조

| 경로 | 내용 |
|---|---|
| `module.prop` | `litegapps_variant=core`, `litegapps_arch=x86_64`, `litegapps_sdk=36` |
| `files/files.tar.xz` | 앱과 설정 파일. arm64판에는 같은 파일이 최상위 `files.tar.xz`로도 있다. zip 안에서는 deflate로 싸였지만 줄지 않는다(stored 블록) |
| `modules/core/<이름>/<이름>-LiteGapps-Addon-<arch>-<android>.zip` | 덧붙이는 묶음. `module.prop`(`android_arch`, `android_sdk`), `system/` 아래 파일, 지울 앱 목록(`list-rm`) |
| `META-INF/`, `customize.sh`, `bin/<arch>/{xz,tar,toybox}`, `*.sh` | 폰의 리커버리나 Magisk가 설치할 때 쓰는 것 |

- `files.tar.xz`는 xz(LZMA2 필터만, 사전 64 MiB, 블록 2개, 블록마다 크기 기록, CRC64)이고, 풀면 267 MiB(x86_64/36)이다. tar 안에는 `<arch>/<api>/system/` 아래 `product/`와 `system_ext/`만 있다. 링크와 장치 파일은 없다.
  - `product/priv-app/GmsCore`(183 MB), `product/priv-app/Phonesky`(90 MB)
  - `product/app/GoogleCalendarSyncAdapter`, `GoogleContactsSyncAdapter`
  - `product/framework/com.google.android.dialer.support.jar`
  - `product/etc/permissions/`(`privapp-permissions-google*.xml`, `com.google.android.gms.xml`, `com.android.vending.xml` 등), `etc/default-permissions/`, `etc/sysconfig/`(`google*.xml`, `pixel_experience_*.xml`, `preinstalled-packages-*.xml`), `etc/preferred-apps/`
  - `system_ext/priv-app/GoogleServicesFramework`, `system_ext/etc/permissions/privapp-permissions-google-se.xml`, `com.google.android.gsf.xml`
- x86_64/36의 덧붙이는 묶음은 Common(권한과 sysconfig 파일의 새 판), GoogleExtShared, GoogleFeedback, GooglePartnerSetup, GoogleRestore, SetupWizard, PixelSetupWizard이다. arm64/37에는 GoogleFeedback이 없다.
- `customize.sh`는 tar의 `<arch>/<api>/system/*`을 시스템으로 복사하고(API 29 이상이면 `product` 아래로), 덧붙이는 묶음을 차례로 깐다. 묶음마다 `android_sdk`와 `android_arch`가 기기와 다르면 거부한다.

### 다시 빌드하지 않고 얹을 때 권한이 서는 조건 (AOSP android16-release)

1. **특권 권한 허용 목록은 앱과 같은 파티션의 것만 본다.** SystemConfig는 `privapp-permissions` 항목을 그 파일의 경로가 `Environment.getProductDirectory()`(`/product/`), `getSystemExtDirectory()`(`/system_ext/`), `getVendorDirectory()`로 시작하는지로 나누어 따로 모은다. `PermissionManagerServiceImpl.checkPrivilegedPermissionAllowlist`는 앱이 있는 파티션의 목록만 본다. `ro.control_privapp_permissions=enforce`이면 목록에 없는 특권 권한은 부팅 때 위반으로 모인다. 그래서 `/product/priv-app`의 GmsCore와 `/product/etc/permissions`의 목록, `/system_ext/priv-app`의 GoogleServicesFramework와 `/system_ext/etc/permissions`의 목록이 각각 같은 경로로 보여야 한다. SystemConfig는 `/product/etc/sysconfig`, `/product/etc/permissions`, `/system_ext/etc/...`를 디렉터리째 읽고, PackagePartitions는 `/product/priv-app`과 `/system_ext/priv-app`을 특권 앱 폴더로 본다. 오버레이를 파티션 자리(`/product`, `/system_ext`)에 그대로 얹으면 어느 경로도 바뀌지 않는다.
2. **기본 권한(`etc/default-permissions`)은 첫 부팅이나 업그레이드 때만 준다.** `Settings.RuntimePermissionPersistence.isPermissionUpgradeNeeded`는 사용자별로 저장해 둔 지문이 `PackagePartitions.FINGERPRINT + "?pc_version=" + 권한 컨트롤러 버전`과 다를 때만 참이고, 그때만 기본 권한 정책이 돈다. `PackagePartitions.FINGERPRINT`는 `ro.build.fingerprint`와 시스템 파티션(system, vendor, odm, oem, product, system_ext)마다의 `ro.<파티션>.build.fingerprint`를 `SystemProperties.digestOf`로 합친 값이다. 그래서 이미 쓰던 사용자 디스크에 Google 앱을 얹기만 하면 지문이 같아, GmsCore와 동기화 앱이 위치, 연락처, 캘린더 같은 기본 권한을 받지 못한다. 새로 나타난 시스템 앱을 사용자 종류별 허용 목록대로 사용자마다 설치하는 일도 업그레이드 경로에서 일어난다.
3. **oem 파티션의 지문은 앱이 보지 않는다.** `Build.getFingerprintedPartitions()`는 bootimage, odm, product, system_ext, system, vendor만 나열한다. `Build.isBuildConsistent()`는 system과 vendor만 비교한다. DSA 이미지에는 oem 파티션이 없으므로 `ro.oem.build.fingerprint`를 정하는 쪽은 DSA뿐이다.

### 결정

1. 받는 것은 Core 빌드다. 목록에서 이름이 `LiteGapps-core-<arch>-<android>-<yyyymmdd>-official.zip`이고 폴더가 날짜 모양인 것 가운데 날짜가 가장 늦은 것을 고른다. 받은 파일의 크기와 MD5가 목록과 같아야 쓴다. `arch`와 API는 실행 중인 게스트가 알리는 `ro.product.cpu.abi`(x86_64는 `x86_64`, arm64-v8a는 `arm64`)와 `ro.build.version.sdk`로 정한다.
2. 이미지에는 tar와 덧붙이는 묶음의 `product/`, `system_ext/` 아래 `app`, `priv-app`, `etc`, `framework`만 넣는다. 파티션 구조를 그대로 두므로 허용 목록이 앱과 같은 파티션에 남는다. 나머지(`vendor`, `lib64`, 설치 스크립트와 도구)는 넣지 않는다. zip, tar, 덧붙이는 묶음 어디에든 `..`나 절대 경로, 드라이브 이름이 있으면 빌드 전체를 거부한다. `module.prop`의 arch나 API가 게스트와 다르거나, 그 arch와 API 아래에 GmsCore와 Phonesky가 없거나, 덧붙이는 묶음의 arch나 API가 다르면 거부한다. 거부하면 저장된 상태는 그대로다.
3. 덧붙이는 묶음 가운데 SetupWizard, PixelSetupWizard, GoogleRestore는 넣지 않는다. 설정 마법사는 HOME 활동으로 DSA 에이전트의 HOME을 밀어내는데, DSA에서는 호스트가 셸이다(D6). GoogleRestore는 마법사가 부르는 복원 단계다. 계정 추가와 Play 로그인은 GmsCore의 자체 화면으로 된다. 나머지(Common, GoogleExtShared, GoogleFeedback, GooglePartnerSetup)는 넣는다.
4. 게스트는 bootconfig에 `androidboot.dsa.gapps=on`이 있을 때만(`dsa-gapps.rc`, `on post-fs-data && property:ro.boot.dsa.gapps=on`) 이미지를 loop 장치로 올린다. 레이블은 `context=u:object_r:system_file:s0`으로, 원래 `/product`와 `/system_ext`의 앱과 설정 파일과 같다. 그 위에 overlayfs로 `lowerdir=/mnt/dsa-gapps/product:/product`를 `/product`에, `system_ext`도 같은 방식으로 얹는다. zygote와 system_server가 시작하기 전이다.
5. 같은 조건에서 product `build.prop`이 `/system/etc/dsa/gapps-on.prop`을 가져와 `ro.oem.build.fingerprint`를 정한다(조건이 없으면 아무것도 정하지 않는 `gapps-off.prop`). 그러면 `PackagePartitions.FINGERPRINT`가 바뀌어 그 부팅이 업그레이드가 된다. 패키지 관리자는 시스템 앱을 다시 훑고, 새 앱을 사용자마다 설치하고, 기본 권한을 준다. 앱이 읽는 지문은 바뀌지 않는다. 제거하면 값이 사라져 다시 업그레이드가 되고, 사라진 앱이 정리된다. 가져오는 줄은 ARM 번역의 가져오는 줄 바로 앞에 둔다(ARM 쪽이 끝줄이어야 한다).
6. ABI 순서: `arm-translation-on.prop`의 목록은 `x86_64,arm64-v8a`이다. 패키지 관리자와 Play는 목록의 앞쪽 ABI를 고르므로, 번역기를 켜도 Play 서비스와 시스템 앱의 갱신은 x86_64판으로 온다. 이미지 검사가 x86_64이 맨 앞인지 확인한다.
7. erofs 이미지는 호스트가 직접 쓴다(`dsa/host/core/src/erofs.rs`). 압축하지 않는 평면 배치, 32바이트 inode, 디렉터리와 일반 파일만 쓰고, 파일 데이터는 도착하는 대로 쓰고 inode와 디렉터리는 마지막에 데이터 뒤에 쓴다. zip은 APK를 읽던 코드(`apk_info::zip`, `inflate`)로, `files.tar.xz`는 새로 쓴 xz/LZMA2 해독기(`gapps/xz.rs`, xz-embedded의 해독 방식)와 흘려 읽는 tar 해석기로 읽는다. MD5도 직접 계산한다. Windows, Linux, macOS가 같은 코드를 쓰고, mkfs.erofs나 xz, Python 같은 도구가 필요 없다. 받기는 이미지 업데이트가 쓰는 `Fetch`(Windows는 WinHTTP, Linux와 macOS는 curl)를 그대로 쓴다. Google의 ARM 번역기로 묶음을 만들 때(R14) 쓰는 `Builder`는 트리 전체를 메모리에 두므로, 수백 MiB인 Google 앱에는 파일을 받는 대로 쓰는 `Stream`을 같은 모듈에 두고 폴더 배치와 inode, 슈퍼블록 부호화를 함께 쓴다.
8. 인증되지 않은 기기: DSA의 안드로이드는 Google이 인증한 기기가 아니므로 Google 로그인이 막힐 수 있다. 사용자는 `https://www.google.com/android/uncertified/`에 Google Services Framework Android ID를 등록한다. 에이전트가 GSF의 `content://com.google.android.gsf.gservices`에서 `android_id`(10진수)를 읽어 호스트에 알리고, `dsa gapps status`와 설정 앱이 보여 준다.

### 검증

- 해독기의 논리는 같은 코드를 줄 단위로 옮긴 Python 시험판(`.scratch/`)으로, `xz` 5.x가 만든 시험 파일 여섯 개(기본, 블록 4 KiB, `-9e`와 SHA-256 검사, `lc=1,lp=2,pb=1`, `preset=0`과 검사 없음, 청크 둘 이상)를 풀어 원본과 같음을 확인했다. 같은 파일들이 `gapps/testdata/`에서 Rust 테스트로 돈다.
- erofs 이미지는 테스트 안의 읽기 코드로 되읽고, Linux CI에서는 erofs-utils의 `fsck.erofs --extract`로 풀어 내용을 비교한다(`fr32_erofs_utils_read_the_image_back_when_they_are_installed`).

### 대가

- 설치하거나 제거한 뒤의 첫 부팅은 업그레이드라 오래 걸린다(모든 패키지를 다시 훑고 컴파일 상태를 맞춘다).
- 서비스는 만드는 동안 압축된 `files.tar.xz`(약 100 MB)와 해독 사전(64 MiB)을 메모리에 둔다. 받은 zip과 만든 이미지는 서비스 폴더에 잠시 있고, 끝나면 지운다. 이미지(270 MB 안팎)는 사용자 디스크에 한 벌 남는다.
- MD5만 공개되므로, 무결성은 받은 파일이 목록과 같다는 데까지다. 목록과 파일이 같은 곳(SourceForge)에서 오므로 배포처 자체가 바뀐 것은 막지 못한다.

### 확인하지 못한 것 (수동 확인)

- 실제 게스트에서 overlay 위의 GmsCore, Phonesky, GoogleServicesFramework가 특권 권한과 기본 권한을 받는지(`dumpsys package com.google.android.gms`의 granted 목록), Play 로그인과 캘린더, 주소록 동기화가 되는지.
- overlayfs가 init의 자격으로 아래 파일을 읽을 때 SELinux 거부가 없는지(`dmesg | grep avc`).
- 에이전트가 `READ_GSERVICES` 권한 없이 gservices에서 `android_id`를 읽을 수 있는지. 못 읽으면 ID는 비어 있고 설정 앱은 안내만 보인다.
- ARM 번역기를 켠 채 Play가 Play 서비스를 x86_64판으로 갱신하는지.

## R16. 표준 GitHub 러너에서 Digitalis 빌드하기 (FR-25, FR-27)

R9는 Digitalis를 기본 번역기로 넣지 못한 이유로 빌드 비용을 들었다. 트리 전체가 `out/`까지 약 300 GB이기 때문이다. 이 절은 GitHub의 표준 러너(ubuntu-24.04, 4 vCPU, 16 GB RAM, 작업당 6시간, 기본 여유 디스크 약 14 GB)만으로 Android 16과 17의 번역기를 빌드하는 방법을 조사한 기록이다. 2026-11-07에 확인했다.

### 확인한 자료

- Digitalis 사이트의 글 "Digitalis on AOSP 17, and the end of android-latest-release"(2026-10-30)와 "Eight projects building on Digitalis"(2026-10-27). `android16`, `android17` 가지가 있고 `android-latest-release`는 더 이상 쓰지 말라고 한다.
- `DigitalisX64/manifest`의 `default.xml`과 `digitalis.xml`(`android16` c04ff67, `android17` 9cd1750). AOSP 쪽은 `android16-qpr2-release`, `android17-release`를 따른다. Digitalis는 `frameworks/libs/binary_translation`, `device/generic/goldfish`를 자기 저장소로 바꾸고 `digitalis`, `sample/hellodigitalis`를 더한다. GitHub 원격을 `git@github.com:`(ssh)으로 적는다.
- `platform_frameworks_libs_binary_translation`(`android17` abefbe2, `android16` 8ab8e93)의 `Android.bp`, `berberis_config.mk`, `enable_arm64_to_x86_64.mk`, `prebuilt/`. AOSP `native_bridge_support`(`android17-release`)의 `native_bridge_support.mk`와 `android_api/`.
- Digitalis 저장소의 `scripts/build-and-package-prebuilts.sh`, `docker/build-digitalis.sh`, goldfish의 `sdk_phone64_x86_64_digitalis.mk`와 `board/emu64xa/BoardConfig.mk`.
- `icy-person/berberis-android17`의 `.github/workflows/google-berberis-arm64.yml`과 최근 40회 실행 기록.
- `denialwm/droidloom`의 `docs/BUILDING.md`, `android/manifest/README.md`, `m2-sparse-source-lock.json`, `native-bridge-lock.json`, `android/native-bridge/product.mk`.
- AOSP `build/soong`의 `ui/build/config.go`, `ui/build/ninja.go`, `cc/config/global.go`, `build/release`의 플래그 값, `build/make/target/product/module_common.mk`(두 가지 모두).

### berberis-android17의 동기화가 실패하는 이유

최근 40회 실행(2026-09-29부터 2026-11-07까지 매일)이 모두 "Sync Android PDK" 단계에서 끝났다. 단계는 결론 없이 40분에서 95분 뒤에 멈추고, 작업의 주석은 거의 매번 러너 자신의 로그를 쓰지 못한 `System.IO.IOException: No space left on device`다. 한 번은 "The hosted runner lost communication with the server"였는데, 이것도 디스크가 찬 러너에서 흔한 증상이다. 원인은 디스크다.

- `repo sync -g pdk`의 `pdk` 그룹은 생각보다 크다. `android17` 매니페스트의 1,087개 프로젝트 중 852개(`android16`은 1,013개 중 811개)가 `pdk`에 든다. darwin용 Clang, 두 가지 Rust 툴체인, NDK, SDK, VNDK 스냅숏, `frameworks/base`까지 들어 있어 사실상 트리 전체다.
- `--partial-clone --clone-filter=blob:limit=5M`은 받을 때만 큰 파일을 미룬다. `repo`가 작업 트리를 모두 체크아웃하므로 미룬 파일도 결국 받아 쓴다. 디스크를 줄이지 못한다.
- 러너의 미리 깔린 SDK와 툴체인을 지우지 않아 루트 디스크의 여유가 약 14 GB에서 시작한다.
- 네트워크나 빠진 프로젝트 문제는 아니었다. 실패한 실행 어느 것도 받기 오류를 남기지 않았다.

이 워크플로는 동기화를 넘었더라도 쓸 수 있는 번역기를 만들지 못한다. `sdk_phone64_x86_64_riscv64`로 lunch하므로 네이티브 브리지 게스트가 riscv64이고, arm64 게스트 라이브러리(`.native_bridge` 변형)는 만들어지지 않는다. 또 지금 쓰지 말라고 한 `android-latest-release`를 따른다.

### droidloom의 방법

droidloom은 Digitalis에서 갈라진 번역기 Teto를 큰 자체 러너(32 GB RAM, 논리 CPU 16개, 여유 120 GiB)에서 빌드한다. 고르는 방법은 이렇다.

- `repo sync`를 쓰지 않는다. 고정한 AOSP 슈퍼프로젝트의 gitlink에서 프로젝트마다 정확한 커밋을 잡아 잠금 파일(`m2-sparse-source-lock.json`, 프로젝트 225개)에 적고, 그 목록만 받는다.
- 큰 프로젝트는 git의 cone 모드 부분 체크아웃(`sparse_paths`)으로 필요한 폴더만 받는다(`frameworks/base`, `hardware/interfaces`, `prebuilts/sdk`, `prebuilts/tools` 등).
- 프레임워크와 시스템 파티션은 빌드하지 않고 공개 CI 이미지(`aosp_cf_x86_64_only_phone`)의 `system`, `system_ext`, `product`를 쓴다.
- 번역기 제품 설정은 `berberis_config.mk`를 포함하고 `soong_config_set,berberis,translation_arch,arm64_to_x86_64`를 정한다.

DSA가 가져온 생각은 슈퍼프로젝트로 커밋을 고정한 잠금 파일, 부분 체크아웃, 공개 이미지의 재사용이다. 코드는 가져오지 않았고, 스크립트는 이 저장소에서 따로 썼다.

### AXRB와 Refract Native의 경로

두 프로젝트는 번역기의 핵심(디코더, 해석기, 가벼운 JIT, 최적화 JIT, 어셈블러, 번역 캐시)을 CMake와 clang-cl로 AOSP 밖에서 빌드한다. 리눅스 계층(시그널, 스레드, 시스템 호출, 실행 메모리)은 자기 코드로 바꾸고, 네이티브 브리지와 bionic을 쓰지 않는다. AXRB 스스로 "This is a native Windows adapter, not Android NativeBridge"라고 한다.

DSA에 필요한 것은 안드로이드의 네이티브 브리지다. 곧 `libberberis_arm64.so`(NativeBridge 인터페이스), 프록시 라이브러리 22개, 프로그램 실행기, 그리고 bionic을 고친 arm64 게스트 라이브러리(`libc`, `libm`, 링커, `app_process`, 스텁 라이브러리)다. 이것들은 Soong 모듈(`native_bridge_supported`, `.native_bridge` 변형, `soong_config` 변수)과 bionic 소스에 묶여 있다. CMake로 다시 쓰면 Digitalis의 빌드를 따로 하나 유지하는 셈이고, Digitalis가 바뀔 때마다 깨진다. 이 경로는 쓰지 않는다.

### 게스트 쪽 파일과 그 출처

번역기가 기대하는 파일은 `berberis_config.mk`의 `BERBERIS_DISTRIBUTION_ARTIFACTS_ARM64`(74개)다. Refract가 쓰는 Digitalis 배포판의 "74 artifacts"가 이것이다. `ld.config.arm64.txt`는 게스트 라이브러리를 `/system/lib64/arm64`와 `/system/lib64/arm64/bootstrap`에서 찾는다. `native_bridge_support.mk`는 게스트 라이브러리를 둘로 나눈다.

- 고친 게스트 라이브러리(`NATIVE_BRIDGE_MODIFIED_GUEST_LIBS`): `libc`, `libm`, `libEGL`, `libGLESv1_CM`, `libGLESv2`, `libGLESv3`, `libvulkan`, `libandroid`, `libandroid_runtime`, `libbinder_ndk`, `libcamera2ndk`, `libmediandk`, `libnativewindow`, `libnativehelper`, `libjnigraphics`, `libneuralnetworks`, `libaaudio`, `libamidi`, `libOpenSLES`, `libOpenMAXAL`, `libwebviewchromium_plat_support`. 호출을 호스트의 x86_64 라이브러리로 넘기는 스텁이다. 링커(`native_bridge_guest_linker`), `app_process`, `libnative_bridge_vdso`, Digitalis의 `libgui` 스텁도 같다. arm64 이미지의 같은 이름 라이브러리는 진짜 구현이라 브리지를 거치지 않으므로 쓸 수 없다. 이것들은 빌드한다.
- 원래 게스트 라이브러리(`NATIVE_BRIDGE_ORIG_GUEST_LIBS`): `libandroidicu`, `libicu`(와 `libicuuc`, `libicui18n`), `libcrypto`, `libssl`, `libsqlite`, `libcompiler_rt`, `libcutils`, `libutils`, `liblog`, `libz`, `libsync`, `libstdc++`, `libdl`, `libdl_android`, 그리고 의존 라이브러리 `libc++`, `libbase`, `libvndksupport`, `ld-android`. 같은 소스를 arm64로 빌드한 것이라 공개 arm64 빌드의 것과 같은 코드다.
  - ICU 넷(`com.android.i18n` APEX), `libcrypto`와 `libssl`(`com.android.conscrypt` APEX), `libsqlite`(`SYSTEM/lib64`)는 DSA가 고정한 arm64 이미지(FR-27, `inputs.json`)에서 가져온다. 고친 라이브러리 어느 것도 이것들에 링크하지 않으므로 빌드 그래프에서 빠지고, `external/icu`, `external/boringssl`, `external/sqlite`를 받지도 빌드하지도 않는다.
  - 나머지(`libcutils`, `liblog`, `libbase`, `libz`, `libc++`, `libdl`, 링커와 짝인 `ld-android` 등)는 고친 `libc`와 링커가 어차피 빌드해 링크하므로 함께 빌드한다. bionic 내부 인터페이스와 짝이 맞아야 하는 것들이라 이미지의 것을 섞지 않는다.
  - arm64 이미지는 `aosp_cf_arm64_only_phone`(armv8-a)이고, Digitalis 보드도 `TARGET_NATIVE_BRIDGE_ARCH_VARIANT := armv8-a`다.
- 프록시 라이브러리(x86_64)는 진짜 프레임워크 라이브러리(`libandroid_runtime`, `libmediandk`, `libEGL` 등 18개)에 링크한다. 이것들을 빌드하면 `libhwui`, Skia, 미디어 스택까지 프레임워크 대부분을 빌드하게 된다. 그래서 고정한 x86_64 이미지에서 꺼내 `cc_prebuilt_library_shared`(`prefer: true`)로 링크한다. 프록시는 바로 그 이미지 위에서 돌므로 정확히 같은 라이브러리에 링크하는 셈이다. 헤더는 해당 프로젝트에서 `include` 폴더만 받아 하나의 헤더 모듈로 내보낸다.

### 빌드 설계

- 잠금 파일 `dsa/guest/arm-translation/digitalis-lock.json`: 버전마다 Digitalis 매니페스트 커밋, AOSP 슈퍼프로젝트 커밋, 받을 프로젝트(Android 17은 53개, 16은 51개)와 각 커밋. AOSP 프로젝트의 커밋은 슈퍼프로젝트의 gitlink에서, Digitalis 프로젝트의 커밋은 가지 끝에서 `update-lock.py`가 잡는다. 받기는 GitHub 원격을 https로 바꾼다. 러너에는 ssh 키가 없다.
- 받기(`.github/scripts/translator/fetch-source.py`): `repo` 대신 프로젝트마다 고정 커밋 하나를 `--depth=1`로 받는다. 부분 체크아웃 프로젝트는 `--filter=blob:none`과 sparse checkout으로 필요한 파일의 블롭만 받는다. Clang은 `build/release`의 `RELEASE_BUILD_CLANG_VERSION`(16: `clang-r563880c`, 17: `clang-r584948b`)이 고른 한 판만 받는다. 매니페스트의 linkfile을 만들고 각 `.git`은 지운다.
- 제품: goldfish 전화 제품 대신 AOSP가 모듈만 빌드할 때 쓰는 `module_common.mk`(Soong만 쓰는 빌드)와 `core_64_bit_only.mk`에서 시작한 `dsa_translator` 제품을 넣는다. 보드는 x86_64 단독에 `TARGET_NATIVE_BRIDGE_ARCH := arm64`(emu64xa와 같다)다. 제품 설정이 기기와 앱 makefile을 읽지 않는다.
- 빌드(`build.sh`): `ALLOW_MISSING_DEPENDENCIES=true`, `SOONG_ONLY=true`, `SOONG_NINJA=ninja`(17은 기본이 siso다), `SKIP_ABI_CHECKS=true`, ccache(`CC_WRAPPER`, `ALLOW_NINJA_ENV`). 모듈 이름 대신 묶음에 들어갈 설치 경로 67개와 `berberis_arm64_host_tests`만 빌드 대상으로 준다. `-k`로 한 번에 오류를 모두 모으고, 실패하면 찾지 못한 모듈과 경로를 요약한다.
- 시험: 호스트 테스트를 돌리고 XML 보고서를 남긴다.
- 묶음: `assemble-digitalis.py collect`가 빌드 산출물과 arm64 이미지의 라이브러리를 아키텍처를 확인하며 모으고, `make-package.py`가 erofs `system.img`와 `arm-translation.json`(이름 Digitalis, 버전 `<major>-<번역기 커밋 12자>`, Apache-2.0, `arm64-v8a`)을 만든다. Digitalis의 `LICENSE`와 받은 커밋 목록을 `system/etc/digitalis/`에 넣는다.
- 캐시: ccache만 버전마다 3 GB까지 GitHub 캐시에 둔다. 실패하거나 시간이 다 된 실행도 저장하므로 다음 실행이 이어 간다. 소스는 캐시하지 않는다. 버전마다 수 GB라 저장소 전체 한도(약 10 GB)를 넘고, googlesource에서 다시 받는 시간이 캐시를 내려받는 시간과 비슷하다.
- 배포: 이미지 배포 워크플로가 develop에서 성공한 가장 최근 "Translator build" 실행 중 지금 잠금의 버전을 가진 것을 찾아, x86_64 이미지마다 `arm-translation.zip`을 배포물에 함께 올리고 `image.json`의 `arm_translation`에 적는다. arm64 이미지는 받지 않고, `make-image.py`와 `check-image.py`가 그것을 막는다.

### 단계별 예상 (실측 전, 버전 하나, 러너 하나)

| 단계 | 디스크 | 시간 |
|---|---|---|
| 러너 정리(SDK, 툴체인, Docker 이미지 삭제) | 루트 여유 약 14 GB에서 약 45 GB 이상으로, `/mnt`가 더 크면 그쪽에 트리 | 2 ~ 4분 |
| 고정 이미지 둘 받기와 라이브러리 꺼내기 | 잠시 약 6 GB(target_files, otatools, dexpreopt 두 벌), 끝나면 수십 MB | 3 ~ 6분 |
| 소스 받기(프로젝트 51 ~ 53개) | 약 10 ~ 13 GB(Clang 한 판 약 3 GB, Rust 툴체인 한 판 약 1.5 ~ 2 GB, build-tools 약 1.5 GB, Go 약 0.5 GB, 나머지) | 10 ~ 25분 |
| Soong 부트스트랩과 분석 | `out/` 1 ~ 2 GB, RAM 4 ~ 8 GB | 10 ~ 20분 |
| 컴파일(번역기를 기기용과 호스트용으로 두 번, arm64와 x86_64 bionic, libc++ 등) | `out/` 15 ~ 25 GB, ccache 최대 3 GB | 처음 2 ~ 3.5시간, ccache가 차면 30 ~ 60분 |
| 호스트 테스트(약 3,700개) | 작다 | 5 ~ 20분 |
| 묶음 만들기와 올리기 | 묶음 약 50 ~ 100 MB | 2분 |

최대 사용량은 소스와 `out/`과 ccache를 합해 약 30 ~ 40 GB다. 첫 실행이 6시간을 넘어도 ccache가 남으므로 다시 실행하면 이어 간다. 그래도 안 되면 단계를 나눈다(분석과 컴파일을 다른 작업으로).

### 결정

- Digitalis를 Soong 안에서, 잠금 파일로 고른 줄인 트리로, 표준 러너에서 빌드한다. CMake 재작성은 하지 않는다.
- 고친 게스트 라이브러리와 번역기는 빌드하고, ICU, BoringSSL, SQLite는 arm64 이미지에서, 프록시가 링크하는 프레임워크 라이브러리는 x86_64 이미지에서 가져온다.
- 묶음은 x86_64 이미지의 배포물에 기본 번역기로 함께 싣고, arm64 이미지에는 싣지 않는다.

### 첫 실행(run 42385312685, android17)과 고친 것

러너 정리, 이미지 라이브러리 꺼내기, 소스 받기는 통과했다. 빌드 단계는 7초 만에 끝났다. `soong_ui`를 만드는 microfactory가 `github.com/akrylysov/pogreb`를 `external/pogreb`에서 찾는데 그 프로젝트가 없었다(`build/soong/scripts/microfactory.bash`의 `-pkg-path`). 빌드가 그 앞에서 멈추었으므로 Soong 분석의 오류는 하나도 드러나지 않았다.

그래서 한 번에 모두 찾도록 빌드 없이 트리를 흉내 냈다. 잠금의 프로젝트마다 빌드 파일(`*.bp`, `*.go`, `go.mod`, `*.mk`, `*.bash`)만 같은 부분 체크아웃 규칙으로 받아(버전마다 약 230 MB), 다음 셋을 확인했다.

1. `soong_ui`의 Go 패키지(microfactory의 `-pkg-path` 대응)와 `soong_build`에 들어가는 `bootstrap_go_package`(플러그인 포함)의 의존이 모두 트리에 있는가.
2. `.bp` 파일이 쓰는 모듈 형식이 모두 트리의 Go 코드에 등록되어 있고, `soong_config_module_type_import`의 `from` 파일과 `build = [...]`가 모두 있는가.
3. 묶음의 대상 모듈에서 시작해 의존(`*_libs`, `defaults`, `rustlibs`, `required`, `srcs`의 `:모듈`, `//경로:모듈` 등)을 따라갔을 때 정의가 없는 모듈은 무엇인가.

찾은 것과 더한 것:

- `external/pogreb`: `soong_ui`와 blueprint(`pogreb`, `pogreb-fs`). 16과 17 모두.
- Rust: 17의 bionic `libc`는 `libicu4x_bionic`(Rust, ICU4X 상자 29개), `libbionic_panic_abort`, `liblibc_alloc_rust`(`system/librustutils/no_std`)를 정적으로 넣고, 게스트 링커(`linker_bin_template`)도 뒤의 둘을 넣는다. 16도 `libicu4x_bionic`을 쓴다. 그래서 Soong이 정한 Rust 툴체인 한 판(17은 `prebuilts/rust-toolchain/linux-x86/1.93.1`, 16은 `prebuilts/rust/linux-x86/1.88.0`과 그 Soong 플러그인)과 `external/rust/android-crates-io`의 상자 29개만 받는다. 잠금의 `{rust}`는 `build/soong/rust/config/global.go`의 `RustDefaultVersion`으로 바뀐다.
- `external/google-highway`(17): x86_64 `libc`의 SIMD 문자열 함수가 `hwy_headers_only`를 쓴다.
- `system/timezone/output_data`: 게스트 `libc`와 x86_64 `libc`가 `required`로 `tzdata_prebuilt`, `tz_version_prebuilt`를 찾는다. 빠지면 그 모듈의 빌드가 실패한다.
- `system/media/audio`: `libhardware_headers`가 `libaudio_system_headers`를 내보낸다. 이것이 없으면 Vulkan 프록시와 번역기 본체가 쓰는 헤더 모듈이 깨진다.
- `packages/modules/common/sdk`와 `system/apex`의 루트 `Android.bp`: `libutils`의 기본값 `apex-lowest-min-sdk-version`이 거기 있고, 그 모듈 형식은 `system/apex/Android.bp`에서 가져온다.

남은 미정의 모듈은 모두 이 빌드가 만들지 않는 변형이다. 진짜 bionic 링커와 debuggerd(`linux_bionic` 호스트용), musl 호스트, Windows 호스트(`-lws2_32`), NDK 변형의 crt 개체, 호스트 `libnativehelper`의 `libdexfile_all_headers`다. `ALLOW_MISSING_DEPENDENCIES`로는 빌드하는 것만 실패하므로 두었다. 이 확인은 정적이라 Soong이 실제로 고르는 변형과 다를 수 있다. 다음 실행의 `m -k` 요약이 나머지를 보인다.

### 두 번째 실행(run 42389737141, android17)과 고친 것

soong_ui는 만들어졌고, 제품 설정(kati)에서 멈추었다. `build/make/core/soong_config.mk`가 `toolchain/pgo-profiles/sampling/afdo_profiles.mk`를 대시 없는 `include`로 읽는데, 그 프로젝트가 없었다(`Error getting 'pre product config' release config`). AFDO 목록은 `libc` 같은 모듈의 프로파일(`fdo_profile`)을 가리키므로 `sampling` 폴더째 받는다. 저장소 이름은 `platform/`이 붙지 않은 `toolchain/pgo-profiles`다.

이번에는 kati가 읽는 makefile도 흉내 냈다. `config.mk`, `envsetup.mk`, `product_config.mk`, `board_config.mk`, `soong_config.mk`, `release_config.mk`와 제품의 `inherit-product` 사슬에서 대시 없는 `include`와 `inherit-product`를 따라가, 트리에 없는 파일을 찾았다(16과 17 모두 40개를 읽었다). 없는 파일은 위의 하나뿐이었다. 변수로 이름을 짓는 include(`$(board_config_mk)`, `$(INTERNAL_BUILD_ID_MAKEFILE)` 등)는 모두 트리 안의 파일이다. 제품 설정에서 경로 때문에 멈추는 `$(error ...)`는 VNDK 판 검사뿐인데, 이 제품은 그 값을 정하지 않는다.

Soong 분석 단계는 흉내 낼 수 없는 부분이 남는다. 이 트리의 Java, APEX, 파일 시스템 모듈(`android_app` 15개, `runtime_apex`, `soong_filesystem_creator` 등)은 빌드하지 않지만 분석은 된다. 그 단계에서 의존 누락이 아닌 오류(싱글톤의 검사 등)가 나면 다음 로그에 나온다. 앞의 두 실행에서 로그 artifact가 비었던 것은 `.dsa-targets`와 `.dsa-sources.json`이 점으로 시작해 `upload-artifact`가 건너뛰었기 때문이다. `include-hidden-files`를 켜고 `soong_ui.log`와 `soong.variables`를 더했다.

### 세 번째 실행(run 42392409794, android17)과 고친 것

제품 설정과 release config는 통과했다(`lunch`가 `TARGET_PRODUCT=dsa_translator`, `BUILD_ID=CP2A.260605.016`, `SOONG_ONLY=true`를 보였다). 그다음 soong_ui가 PATH를 꾸미는 단계(`ui/build/path.go`)에서 멈추었다. `build.sh`가 정한 `TEMPORARY_DISABLE_PATH_RESTRICTIONS`는 이제 쓰지 않는 값이라, 정해져 있으면 soong_ui가 곧바로 끝낸다. 그래서 Soong 부트스트랩과 분석은 시작하지 않았다. 이 오류는 `error.log`가 아니라 콘솔과 `soong.log`에만 남으므로, 실패하면 `soong.log`의 끝도 보인다.

그 값을 빼면 PATH 제한이 켜진다. 빌드가 쓰는 `python3`와 기본 명령은 `prebuilts/build-tools/path/linux-x86`에 있고(잠금이 `path` 폴더를 받는다), ccache는 `CC_WRAPPER`에 절대 경로로 주므로 제한에 걸리지 않는다. soong_ui가 환경 변수 때문에 멈추는 다른 경우(JDK 실험 값, `BUILD_USERNAME` 없음)도 `build.sh`에 없다.

### 네 번째 실행(run 42394768870, android17)과 고친 것

처음으로 Soong까지 갔다. soong_ui, blueprint, `soong_build`(플러그인 포함)가 약 58초 만에 빌드되었고, Android.bp 분석이 끝까지 돌아 모듈마다 빌드 동작을 만드는 단계(`GenerateBuildActions`)에서 멈추었다. Go 의존과 모듈 형식에서는 오류가 없었다.

멈춘 곳은 `dex_bootjars`의 패닉이다(`Failed to lookup dex2oatd dependency`, `build/soong/dexpreopt/config.go`). `module_common.mk`가 기본 ART 설정을 가져오고, 보드가 dexpreopt를 끄지 않아서(`WITH_DEXPREOPT ?= true`), Soong이 부트 이미지를 만들려고 dex2oat를 찾았다. 트리에 ART가 없으므로 의존이 없고, 패닉은 다른 오류를 모으기 전에 분석 전체를 끝낸다. 그래서 이번 로그에는 이 하나만 있다.

보드 설정에 `WITH_DEXPREOPT := false`를 넣었다. 그러면 `dex_preopt_config.mk`가 앱과 부트 이미지의 dexpreopt를 모두 끄고(`DisablePreopt`, `DisablePreoptBootImages`), `IsDex2oatNeeded`가 거짓이 되어 dex2oat를 찾는 곳이 모두 빈 값을 받는다. Soong 안에서 dex2oat를 찾는 다섯 곳(`dexpreopt.go`, `dexpreopt_bootjars.go`)이 모두 이 검사를 거친다. 같은 이유로 프레임워크가 없는 트리에서 숨은 API 목록을 만들지 않도록 `UNSAFE_DISABLE_HIDDENAPI_FLAGS=true`를 둔다.

### 다섯 번째 실행(run 42396287578, android17)과 고친 것

dexpreopt를 끈 뒤 `dex_bootjars`는 지나갔고, 분석이 다시 `GenerateBuildActions`에서 패닉으로 멈추었다. 이번에는 `soong_filesystem_creator`가 제품으로부터 만든 `dsa_translator_generated_device`(`android_device`)가 target_files의 메타데이터를 모으다 없는 모듈의 출력을 읽었다(`android_device.go`의 `copyMetadataToTargetZip`, nil 포인터). Soong-only 빌드는 제품마다 기기와 파티션 이미지 모듈을 스스로 만드는데(`build/soong/fsgen`), 이 트리에는 그 재료가 없다.

`fsgen`은 묶지 않은(unbundled) 빌드에서는 아무것도 만들지 않는다(`shouldEnableFilesystemCreator`). 그래서 `build.sh`가 `TARGET_BUILD_UNBUNDLED=true`를 둔다. AOSP가 메인라인 모듈을 부분 트리에서 따로 빌드할 때(`module_*` 제품, `ALLOW_MISSING_DEPENDENCIES`)와 같은 방식이라, 기기 단위의 싱글톤(기기, 파티션, 부트 이미지, dist)이 꺼진 상태로 쓰이고 시험되는 경로다. `unbundled_builder`는 `TARGET_BUILD_APPS`가 비어 있으면 아무것도 하지 않는다. cc와 Rust 빌드는 이 값에 따라 달라지지 않고(`cc`, `rust`, `genrule`에 `UnbundledBuild()`를 보는 곳이 없다), 설치는 그대로 `out/target/product/dsa_translator`로 간다. 함께 정해지는 `TARGET_BUILD_USE_PREBUILT_SDKS`는 Java SDK에만 쓰인다.

앞을 내다보고 이 트리의 위험한 모듈도 살폈다. 기기 단위 모듈은 `build/make/target/product/generic`과 `gsi`의 `android_system_image`, `bionic/apex`의 `runtime_apex`, `sdk`, `module_exports`, `build/soong`의 `product_config`, `build_prop` 정도이고, 모두 메인라인 모듈의 부분 트리 빌드에도 있는 것들이다. dexpreopt는 이미 꺼져 있다.

### 여섯 번째 실행(run 42397789226, android17)과 고친 것

패닉 없이 분석이 끝까지 돌았다(`[100% 256/256]`). 오류는 하나였다. `bionic/apex/Android.bp`의 `com.android.runtime`(런타임 APEX)이 `system/sepolicy/apex/com.android.runtime-file_contexts`를 찾지 못했다. 분석의 다른 단계에서는 오류가 없었다. 앞에서 걱정한 `build/make/target/product/generic`과 `gsi`의 `android_system_image`는 이 단계를 지나갔으므로 그대로 둔다.

런타임 APEX에서 나오는 것은 묶음에 하나도 없다. 대상 파일에서 의존을 따라가도 `bionic/apex`의 모듈(`com.android.runtime`, 그 키와 인증서, `runtime-module-sdk`, `runtime-module-host-exports`, `bionic-linker-config`)에 닿지 않는다. 다른 `.bp`가 이 파일에서 모듈 형식을 가져오지도 않는다(`bionic/libc`의 `visibility`에 `//bionic/apex`가 있지만, Soong은 보이는 범위에 적힌 패키지가 있는지 묻지 않는다). 트리에 있는 APEX는 이것 하나다. 그래서 `system/sepolicy`와 APEX 키를 더하는 대신 이 파일을 트리에서 뺀다. 잠금에 `exclude`(체크아웃 뒤 지우는 파일)를 더했고, bionic이 `apex/Android.bp`를 뺀다.

### 일곱 번째 실행(run 42399261076, android17)과 고친 것

분석이 통과하고 컴파일이 시작되었다. 실패한 동작 1,663개 중 1,652개는 컴파일러 앞의 ccache가 `/home/runner/.cache/ccache/lock`을 만들지 못한 것이다(`Read-only file system`). soong_ui는 ninja를 nsjail 안에서 돌리고, 그 안에서 쓸 수 있는 곳은 `/tmp`와 `out/`뿐이다(`ui/build/sandbox_linux.go`의 `-B`). 그래서 캐시를 `/tmp/dsa-ccache`로 옮겼다. 워크플로의 캐시 복원과 저장도 같은 폴더를 쓰고, `build.sh`는 캐시가 그 두 곳 밖에 있으면 시작하지 않는다. 새 "Framework build" 워크플로도 같은 스크립트 구조라 함께 고쳤다(그쪽 `build.sh`에 남아 있던 `TEMPORARY_DISABLE_PATH_RESTRICTIONS`도 뺐다).

나머지 11개는 Python 빌드 도구(`gen_asm`, `gensyscalls`, `gen_intrinsics` 등)의 `missing dependencies: libsqlite, aprotoc`다. Soong은 모든 `python_binary_host`에 실행기용 `libsqlite`와 protobuf 컴파일러 `aprotoc`를 의존으로 붙인다(`build/soong/python/python.go`). 쓰는 실행기는 `prebuilts/build-tools`의 프리빌트(`py3-launcher-autorun64`)인데, 이 파일의 `DT_NEEDED`는 glibc 라이브러리뿐이고 SQLite를 링크하지 않는다. 이 빌드의 Python 도구에는 `.proto` 소스가 없다. 진짜를 빌드하려면 `external/sqlite`가 호스트 ICU(`external/icu`)를, `external/protobuf`가 abseil을 끌고 온다. 그래서 제품 폴더에 대역을 둔다. 빈 호스트 공유 라이브러리 `libsqlite`와, 실행되면 이유를 말하고 실패하는 `aprotoc`(`sh_binary_host`)다.

ccache 때문에 컴파일이 모두 실패해서, 진짜 컴파일 오류는 이번 로그에 아직 하나도 없다.

### 여덟 번째 실행(run 42402218071, android17)과 고친 것

ccache가 동작했고 컴파일이 약 19분 돌았다. `-k`로 모인 실패는 15개다. 14개는 프록시 라이브러리와 `libberberis_native_activity_arm64`가 프레임워크의 헤더(`android/rect.h`, `media/NdkImageReader.h`, `vulkan/vk_layer_interface.h`, `SLES/OpenSLES.h`, `OMXAL/OpenMAXAL.h`, `aaudio/AAudio.h`, `amidi/AMidi.h`, `camera/NdkCaptureRequest.h`, `android/binder_ibinder.h`, `android/hardware_buffer.h`, `android/native_window.h`, `android_runtime/AndroidRuntime.h`)를 찾지 못한 것이다. 헤더 파일은 모두 트리에 있었다. 컴파일 명령의 `-I` 목록에 그 폴더들이 없고 대신 `device/dsa/dsa_translator/headers`가 있었다. 헤더 모듈의 `export_include_dirs`에 적은 `../../../../frameworks/...`가 모듈 폴더 자체로 바뀐 것이다. 모듈은 자기 폴더 밖을 내보낼 수 없다.

그래서 `build.sh`가 내보낼 include 폴더(`headers/include-dirs.txt`, 18개)를 트리에서 `device/dsa/dsa_translator/headers/` 아래 같은 경로로 복사하고, 헤더 모듈은 그 복사본을 내보낸다. 목록의 폴더가 트리에 없으면 빌드 전에 멈춘다.

나머지 하나는 소스에서 빌드한 x86_64 `libnativehelper`의 링크였다. 버전 스크립트 `stub.map`이 비어 있었다(`unexpected EOF`). 이 라이브러리는 ART APEX의 것이고 `stubs`(`versions: ["S"]`)를 가진다. 이 축소 트리에서 그 판의 스텁 목록을 만드는 데 무엇이 빠졌는지는 로그로 알 수 없었다. 프록시가 링크하는 것은 게스트의 x86_64 시스템이 실제로 싣는 라이브러리이므로, 다른 프레임워크 라이브러리처럼 고정한 x86_64 이미지의 `com.android.art`에서 꺼내 프리빌트로 쓴다. 헤더(`libnativehelper/include`, `include_platform`)는 위의 복사본으로 내보낸다.

컴파일 동작은 다른 라이브러리의 링크를 기다리지 않으므로, 이번 로그에 모든 컴파일 오류가 이미 나왔다. 실패한 모듈에 링크하는 모듈의 링크는 시도되지 않았으므로, 다음 실행에서 링크 오류가 처음 보일 수 있다.

### 남은 위험과 확인할 것

- 프로젝트 목록은 읽어서 만든 첫 추정이다. 빠진 모듈은 빌드 로그의 "missing dependencies"로 드러나고, 그 프로젝트를 잠금에 더한 뒤 `update-lock.py --missing`으로 고정한다. 특히 Soong 플러그인(`aidl_interface` 같은 모듈 형식), Python 실행기 프리빌트, Rust(`aconfig`)가 더 필요할 수 있다.
- 헤더만 받는 프로젝트를 가리키는 `export_include_dirs`(`../../../../frameworks/...`)를 Soong이 받아들이는지 확인해야 한다. 안 되면 헤더를 제품 폴더로 복사한다.
- Soong만 쓰는 빌드에서 설치 경로를 빌드 대상으로 줄 수 있는지 확인해야 한다. 안 되면 모듈 이름으로 바꾼다.
- FR-25는 기본 묶음이 있는 x86_64 이미지가 처음부터 그 묶음으로 번역하고, 사용자가 다른 묶음으로 바꾼 뒤에도 기본 묶음으로 되돌릴 수 있다고 정한다. 이 절을 쓸 때는 그 호스트와 게스트 쪽 동작이 없었다. 아래 "기본 묶음의 자리와 binfmt_misc"가 그 뒤의 구현이다. 그때는 배포물의 zip을 풀어 `dsa arm-translation install`로 넣어야 한다. 제안하는 방법: x86_64 이미지의 시스템 파티션에 기본 묶음의 `system.img`를 넣고(`/system/etc/dsa/arm-translation/default.img`), 호스트는 사용자 묶음이 없고 설정이 켜져 있으면 `androidboot.dsa.arm_translation=default`를 넣으며, 게스트는 그 값에서 기본 이미지를 얹는다. 사용자 묶음을 지우면 기본으로 돌아간다. 상태 보고(`restart_needed`, 설정 앱의 이름과 버전)도 기본 묶음을 알아야 한다.
- 기본 묶음이 DSA 게스트에서 ARM64 전용 앱을 실제로 돌리는지는 PC에서 확인한다(FR-25의 수동 확인).

### 기본 묶음의 자리와 binfmt_misc (2026-11-08)

위의 제안대로 구현했다. 고른 이유와 확인한 것을 적는다.

- 자리: 기본 묶음의 `system.img`는 x86_64 이미지의 시스템 파티션(`/system/etc/dsa/arm-translation/default.img`)에 들어간다. 호스트가 이미지와 함께 받아 사용자 데이터에 두는 방법도 있었지만 쓰지 않았다. 시스템 파티션에 두면 이미지 업데이트(FR-14)의 서명 확인, 해시 확인, 시험 부팅, 되돌리기를 그대로 탄다. 컴퓨터의 모든 사용자가 한 벌을 함께 쓰고, 사용자마다 디스크로 보낼 필요가 없으며, 이미지와 번역기의 버전이 어긋날 수 없다. 이미지는 묶음 크기(약 50 ~ 100 MB)만큼 커진다.
- 호스트가 아는 법: `image.json`의 `arm_translation`에 `system_image`(그 경로)와 `system_image_sha256`이 있다. 호스트는 x86_64 이미지에서 그 경로를 말하는 항목만 기본 묶음으로 친다. 그래서 PR #373의 배포물처럼 zip만 있고 시스템 파티션에는 없는 이미지는 기본 묶음이 없는 것으로 다룬다.
- 고르기: 사용자가 설치한 묶음(자기 것이나 Google 것)이 있으면 `androidboot.dsa.arm_translation=on`, 없고 기본 묶음이 있으면 `=default`, 설정이 꺼져 있으면 아무것도 넣지 않는다. 묶음을 지우면 기본으로 돌아가므로 "기본 번역기 사용"은 설치한 묶음을 지우는 일이다. 게스트의 사용자 묶음 자리는 하나뿐이라 기본으로 돌아가면서 다른 묶음을 남겨 둘 곳이 없다.
- 다시 시작 안내: 게스트는 번역기가 얹혔는지만 보고하므로, 두 번역기 사이를 바꾼 것(실행 중에 Google 것을 설치한 경우 등)을 알 수 없다. 호스트는 부팅마다 넣은 번역기를 사용자 설정 폴더(`<user>.arm-translation-boot.json`)에 적고, 상태를 낼 때 그것과 지금의 선택을 비교한다. 제어 프로토콜은 바꾸지 않았다.
- `ro.boot.dsa.arm_translation=default`인데 기본 묶음이 없는 이미지는 `arm-translation-default.prop`도 없다. 가져올 파일이 없으므로 `x86_64`만 알린다. 이미지 검사는 arm64 이미지가 기본 묶음이나 그 속성 파일을 가지면 실패한다.

binfmt_misc: PR #373은 `dsa-arm-translation.rc`가 binfmt_misc를 마운트하지 않는다고 지적했다. 고정한 x86_64 입력(`aosp_cf_x86_64_only_phone-target_files-15581820.zip`)에서 확인했다.

- `SYSTEM/etc/init/hw/init.rc`에 binfmt_misc를 말하는 줄이 없다. `ro.enable.native.bridge.exec`에 걸린 동작도 없다. 그러므로 아무도 `/proc/sys/fs/binfmt_misc`에 binfmt_misc를 마운트하지 않고, 규칙을 `register`에 쓰는 `copy`는 procfs의 빈 디렉터리에 쓰다 실패한다. 앱이 실행하는 ARM64 프로그램이 번역기로 돌지 않는다.
- 플랫폼 정책(`plat_sepolicy.cil`): 그 디렉터리는 `proc`(`genfscon proc "/"`)이고, init에게 `proc` 디렉터리의 `mounton`이 없다. `neverallow`는 init과 `dexopt_chroot_setup`을 빼 두었으므로 더할 수 있다. 파일 시스템 `mount`는 init의 `fs_type` 규칙이 이미 덮고, `binfmt_miscfs` 파일 쓰기는 벤더 정책에 있다.
- 고친 것: 두 `copy` 앞에 `mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc nodev noexec nosuid`를 넣고, `arm-translation.cil`에 `(allow init proc (dir (mounton)))`와 `(allow init binfmt_miscfs (filesystem (mount)))`를 더했다. 기본 묶음 쪽 동작도 같은 순서다.
- 남은 확인: 실제 부팅에서 `cat /proc/sys/fs/binfmt_misc/arm64_exe`가 `enabled`이고, 앱이 실행하는 ARM64 실행 파일이 도는지는 PC에서 본다.

## R17. 정한 ABI로 설치하기와 ABI를 바꿔 다시 설치할 때의 데이터 (FR-25)

질문: 호스트가 설치하는 APK를 `arm64-v8a`로 설치하게 하려면, 그리고 "ARM으로 실행" 스위치로 이미 설치된 앱을 다른 ABI로 다시 설치하려면 Android 16과 17에서 무엇을 쓰는가. 다시 설치할 때 앱의 데이터는 남는가.

### 확인한 자료

| 자료 | 버전 |
|---|---|
| AOSP `frameworks/base` `core/java/android/content/pm/PackageInstaller.java` | `android16-release`, `android17-release` |
| AOSP `frameworks/base` `services/core/java/com/android/server/pm/PackageManagerShellCommand.java` | `android16-release` |
| 이 저장소의 에이전트(`AndroidPackageInstaller.kt`, `AndroidPackageUninstaller.kt`)와 권한 목록 | `develop` `0d832ed2` |

### 방법

android.googlesource.com의 웹 보기로 소스를 읽었다. 기기에서는 아직 확인하지 않았다.

### 찾은 것

1. SDK에는 ABI를 정하는 공개 API가 없다. Android 16의 `PackageInstaller.SessionParams`는 숨은 공개 필드 `public String abiOverride;`를 가지고, `pm install --abi`도 셸 명령에서 `sessionParams.abiOverride`를 채워 같은 세션 경로로 설치한다. 설치 크기 계산(`InstallLocationUtils.calculateInstalledSize`)도 이 값을 쓴다.
2. `android17-release`의 같은 파일은 웹 보기가 길이 제한으로 잘려 필드 선언 줄을 확인하지 못했다. 앞부분의 `InstallInfo`가 `params.abiOverride`를 쓰는 것은 같다.
3. 에이전트는 플랫폼 키로 서명된 시스템 앱이라 숨은 API 제한을 받지 않는다. 그래서 리플렉션으로 `abiOverride`를 채운다. 필드가 없으면 설치를 거부한다. 조용히 Android가 고른 ABI로 설치하면 사용자가 고른 것과 다른 결과가 나오기 때문이다.
4. 같은 패키지, 같은 서명, 같거나 높은 버전 코드의 전체 설치 세션은 교체 설치다. 교체 설치는 업데이트와 같은 경로라 앱 데이터(`/data/data/<package>`)를 지우지 않는다. 그래서 스위치는 기기에 있는 그 앱의 APK(기본과 분할)를 새 세션에 그대로 써서 ABI만 바꿔 다시 설치한다. 데이터는 남는다.
5. 교체가 거절될 수 있는 경우는 남는다. 같은 공유 사용자 ID를 쓰는 패키지끼리 ABI를 맞춰야 하는 경우(`INSTALL_FAILED_SHARED_USER_INCOMPATIBLE`)와 기존 앱을 교체할 수 없다는 거절(`INSTALL_FAILED_UPDATE_INCOMPATIBLE`)이다. 이때는 제거한 뒤 설치해야 하고, 데이터가 지워진다.

### 결정

- 호스트가 설치하는 APK는 `InstallApk.abi`로 ABI를 정하고, 에이전트가 그것을 세션의 `abiOverride`로 넣는다. 비어 있으면 Android가 정한다.
- "ARM으로 실행"은 교체 설치로 데이터를 지킨다. 위 5의 거절이 오면 아무것도 바꾸지 않고 `REINSTALL_STATUS_DATA_WOULD_BE_ERASED`로 답한다. 설정 앱은 데이터가 지워진다는 확인 창을 띄우고, 사용자가 동의하면 `erase_data`로 다시 요청한다. 에이전트는 APK를 자기 캐시에 복사하고, 앱을 제거하고, 복사본을 설치한다.
- 설치가 끝나면 에이전트가 `primaryCpuAbi`를 다시 읽어, 요청한 ABI가 아니면 성공으로 알리지 않는다.

### 확인하지 못한 것

- Android 17 이미지에서 `SessionParams`에 `abiOverride` 필드가 있는지. 없으면 에이전트 로그에 "this Android's package installer takes no ABI override"가 남고 설치가 거부된다.
- 실제 PC(수동 확인): x86_64와 arm64를 모두 가진 앱을 설치해 `dumpsys package <package>`의 `primaryCpuAbi`가 `x86_64`인지, "ARM으로 실행"을 켜면 `arm64-v8a`로 바뀌고 앱 데이터(로그인, 설정)가 남는지, 끄면 `x86_64`로 돌아오는지 본다. 공유 사용자 ID를 쓰는 앱에서 교체가 거절되는지도 본다. 결과는 이 절에 기록한다.

## R18. 창 관리자(dsa-wm)가 되풀이해 끝난 원인: 렌더러의 장치 손실 abort (NFR-5, INTENT D24)

### 확인한 자료
- 소유자 PC(Intel Arc, Windows 11 26100, DSA 0.1.9)의 `dsa-wm.exe` 크래시 덤프 10개(2026-11-07 10:48 ~ 11-08 01:31 KST). 모두 `0xc0000409`(`FAST_FAIL_FATAL_APP_EXIT`), `ucrtbase!abort`.
- 앞의 6개는 2026-11-06 13:10 UTC에 빌드한 `gfxstream_backend.dll`이고, Graphics test 42222816694의 PDB와 DLL이 맞는다. 뒤의 4개는 cfe0b545의 Graphics test 42336149590의 것이 맞는다(PDB GUID `A1BCDDEF…`).
- `dsa-wm.log`, 같은 시각의 게스트 logcat(`.scratch/guest-crash-1108/all.txt`), 응용 프로그램 이벤트 로그의 `Application Error` 기록.

### 방법
- `cdb -z <덤프> -i <DLL 폴더> -y <PDB 폴더> -lines -c ".ecxr; kn"`로 스택을 풀었다. 덤프에 DLL 이미지가 없어서 앞의 6개는 그 빌드의 DLL을 `-i`로 주어야 풀린다.
- `AbortMessage`의 문자열은 덤프의 힙에 없어서 읽지 못했다. 대신 줄 번호로 소스의 abort 자리를 찾았다.

### 찾은 것
- 세 갈래이고, 모두 Vulkan 장치 손실(`VK_ERROR_DEVICE_LOST`)이다.
  - 6개: `on_vkAcquireImageANDROID`(VkAndroidNativeBuffer.cpp 658)의 `VK_CHECK(vkQueueSubmit)` → `on_DeviceLost` → `GFXSTREAM_ABORT`.
  - 3개: `on_vkCreateFence` → `ExternalFencePool::pop`의 `VK_CHECK(vkGetFenceStatus)` → `on_DeviceLost` → abort.
  - 1개(01:31, Chromium 창이 닫힐 때): 프로세스 정리 스레드의 `vkDestroyInstanceImpl` → `~ExternalFencePool`(VkDecoderInternalStructs.h 57). 장치가 손실되어 `vkDeviceWaitIdle`이 실패하자 `destroyDeviceObjects`가 객체를 남기고 돌아갔고, 풀이 펜스를 쥔 채 소멸자에서 abort했다.
- 생성된 디코더는 게스트 호출이 `VK_ERROR_DEVICE_LOST`를 돌려받을 때마다 `on_DeviceLost`를 부르고, 그것이 abort했다. 그래서 장치 하나를 잃으면 창 관리자와 모든 창이 끝났다.
- 앞의 9개는 정확히 분이 바뀌는 순간(`hh:mm:00`)에 끝났다. 게스트에서 처음 죽은 것은 매번 에이전트의 `RenderThread`(`vkAcquireImageANDROID`)였다. 에이전트의 위젯 렌더러가 시계 위젯을 분마다 GPU로 다시 그린다(`AndroidWidgets.kt`의 `scheduleTick`). 이때 호스트 GPU가 리셋되었다. 시스템 로그에는 TDR 이벤트(4101)가 없었다(드라이버의 엔진 단위 리셋으로 보인다). 리셋 자체의 원인은 덤프로는 알 수 없다.
- gfxstream의 로그(`ERR`, abort 메시지)는 표준 오류로만 나가서, 콘솔이 없는 dsa-wm에서는 사라졌다.

### 결정
- 렌더러 패치 0015와 aemu 패치 0002: 게스트를 섬기는 스레드의 치명적 오류는 그 게스트의 스트림만 끝낸다. 장치 손실은 게스트에 `VK_ERROR_DEVICE_LOST`로 돌려주고, 렌더러 자신의 장치는 다음 작업 전에 새로 만들며 ColorBuffer를 같은 핸들로 다시 만든다. 정리 스레드는 손실된 장치의 객체도 지우고, 펜스 풀 소멸자는 끝내지 않는다.
- 렌더러의 경고와 오류, 치명적 오류, Rust 패닉을 `dsa-wm.log`에 남긴다.

### 확인하지 못한 것
- 실제 GPU 리셋에서의 복구는 실제 PC에서 확인해야 한다(NFR-5의 수동 기준). CI는 lavapipe에서 가짜 장치 손실로만 확인한다.
- 시계 위젯의 분 단위 그리기가 왜 Intel 드라이버의 리셋을 부르는지는 확인하지 못했다.

## R19. GPU 장치를 잃었을 때 SurfaceFlinger와 HWUI가 다시 그리기 (NFR-5, D24)

질문: 호스트 GPU가 리셋되어 렌더러가 게스트에 장치 손실(`VK_ERROR_DEVICE_LOST`, `EGL_CONTEXT_LOST`)을 돌려줄 때, DSA 이미지의 SurfaceFlinger와 HWUI는 지금 어떻게 반응하는가. 다시 시작하지 않고 복구하려면 무엇을 고쳐야 하는가. 게스트의 GLES와 Vulkan 드라이버도 고쳐야 하는가. 2026-11-08에 확인했다.

### 확인한 자료

| 대상 | 버전 |
|---|---|
| Android 16 이미지 입력 | `aosp_cf_x86_64_only_phone-userdebug` 빌드 15581820 (`BP4A.251205.006`, 릴리스 설정 `bp4a`, `clang-r563880c`) |
| Android 17 이미지 입력 | 빌드 16373615 (`CP2A.260605.016`, `cp2a`, `clang-r584948b`) |
| 빌드의 소스 | ci.android.com의 `manifest_15581820.xml`, `manifest_16373615.xml`. `frameworks/native` `fcbde2b`(16)와 `ae266dc`(17), `frameworks/base` `45034f0`와 `94b4c16`, `external/skia` `22f5edb`(16), `external/angle` `ff25cbd`(16) |
| 게스트 Vulkan 드라이버 | DSA가 빌드하는 `vulkan.ranchu.so`: gfxstream `d047a57`, mesa3d `224e91e`의 `src/gfxstream/guest/vulkan_enc` |
| 크래시 기록 | 2026-11-07 소유자 PC(Intel Arc)의 Android 16 이미지 logcat과 tombstone |

target_files에서는 `SYSTEM/build.prop`, `SYSTEM/etc/build_flags.json`, `SYSTEM/etc/aconfig_flags.pb`, `VENDOR/etc/init/init_graphics.vendor.rc`를 읽었다. Android 17 것은 2.3 GB zip 전체를 받지 않고 HTTP Range로 필요한 항목만 꺼냈다. 소스는 해당 커밋을 `--filter=blob:none`과 sparse checkout으로 받아 읽었다. 빌드는 하지 않았다.

### 이 이미지들이 쓰는 경로

- SurfaceFlinger는 스레드를 쓰는 SkiaGL RenderEngine(`skiaglthreaded`)을 쓴다. `debug.renderengine.backend`를 정하는 곳이 없고, `bp4a`와 `cp2a`의 aconfig 값 묶음은 `vulkan_renderengine`과 `graphite_renderengine`을 켜지 않는다. tombstone의 `SkiaGLRenderEngine::flushGL`과 스레드 이름 `RenderEngine`이 이를 보인다.
- HWUI는 `SkiaOpenGLPipeline`을 쓴다. `ro.hwui.use_vulkan`이 비어 있고, `init_graphics.vendor.rc`가 `debug.hwui.renderer`에 넣는 `ro.boot.hardware.guest_hwui_renderer`를 DSA가 주지 않는다.
- 두 GL 경로는 모두 ANGLE(`ro.hardware.egl=angle`)을 지나 DSA의 gfxstream Vulkan 드라이버로, Hyper-V 소켓을 거쳐 호스트 렌더러에 닿는다. 그래서 호스트 장치를 잃으면 게스트에는 Vulkan의 장치 손실로 먼저 오고, GL 쪽에는 ANGLE이 그것을 컨텍스트 손실로 바꾸어 전한다.
- Vulkan RenderEngine(Ganesh, Graphite)과 `SkiaVulkanPipeline`은 지금 쓰이지 않지만, 속성 하나로 켜지므로 같은 복구를 넣었다.

### 2026-11-07의 크래시

호스트 렌더러 프로세스가 끝나 연결이 끊긴 경우다.

- 앱의 RenderThread: `EglManager::beginFrame` → ANGLE `WindowSurfaceVk::prepareSwap` → `vkAcquireImageANDROID` → `VulkanStreamGuest::read`의 `abort()`. 연결에서 응답을 읽지 못하면 드라이버가 프로세스를 끝낸다.
- SurfaceFlinger의 RenderEngine 스레드: `mapExternalTextureBuffer` → `GaneshBackendTexture`의 `LOG_ALWAYS_FATAL("Failed to create a valid texture")`, 또는 `SkiaGLRenderEngine::flushGL` → `eglCreateSyncKHR` → `vkCreateFence`에서 같은 `abort()`.
- SurfaceFlinger가 끝나면 system_server와 함께 프레임워크 전체가 다시 시작하고, 열린 앱이 모두 닫힌다.

### 지금의 반응

| 위치 | 장치 손실에서 하는 일 |
|---|---|
| RenderEngine `drawLayersInternal` 시작 | `LOG_ALWAYS_FATAL_IF(context->isAbandonedOrDeviceLost())` |
| RenderEngine `GaneshBackendTexture` 생성과 `makeImage`/`makeSurface` | 텍스처나 이미지를 만들지 못하면 `LOG_ALWAYS_FATAL` |
| RenderEngine `VulkanInterface::onVkDeviceFault`(Skia의 `fDeviceLostProc`) | 16은 `LOG_ALWAYS_FATAL`, 17은 `LOG_THREAD_STATE_AND_CRASH` |
| RenderEngine `GraphiteVkRenderEngine::flushAndSubmit` | `insertRecording`이나 `submit`이 실패하면 끝낸다 |
| `SkiaGLRenderEngine` | `EGL_CONTEXT_LOST`를 보는 곳이 없다. RenderEngine은 `SurfaceFlinger::init`에서 한 번 만들어지고, 다시 만드는 길이 없다. Vulkan 장치(`VulkanInterface`)는 정적 객체다 |
| HWUI `CanvasContext::draw` 시작 | `LOG_ALWAYS_FATAL("Lost GPU device unexpectedly")` |
| HWUI `EglManager::swapBuffers` | `EGL_BAD_SURFACE`, `EGL_BAD_NATIVE_WINDOW`가 아닌 오류는 모두 `LOG_ALWAYS_FATAL`. `EGL_CONTEXT_LOST`도 여기에 든다 |
| HWUI `EglManager::makeCurrent`(오류를 받지 않는 호출), `damageFrame`, `createSurface` | 실패하면 `LOG_ALWAYS_FATAL` |
| HWUI `VulkanManager`의 장치 손실 콜백 | `LOG_ALWAYS_FATAL` |

복구에 쓸 수 있는 것은 이미 있다.

- HWUI의 `RenderThread::destroyRenderingContext`. `trimMemory`가 `TRIM_MEMORY_BACKGROUND` 이상일 때와 `CacheManager`의 컨텍스트 시한에서 부른다. WebView functor에 컨텍스트가 사라진다고 알리고(`onContextDestroyed`), `setGrContext(nullptr)`가 `RenderState::onContextDestroyed`로 각 창의 레이어와 파이프라인의 EGL/Vulkan surface와 TextureView 이미지를 놓은 뒤 `GrDirectContext`를 버리며, `EglManager::destroy`(`eglTerminate`)나 `VulkanManager`를 놓는다. 다음 `makeCurrent`가 컨텍스트와 surface를 다시 만든다(`SkiaOpenGLPipeline::makeCurrent`의 주석이 trimMemory 뒤의 이 경우를 말한다). display list는 남는다.
- HWUI의 `SyncResult::UIRedrawRequired`. 동기화 결과에 이 비트가 있으면 Java의 `ThreadedRenderer`가 `ViewRootImpl.invalidate()`를 불러 UI 스레드가 창 전체를 다시 그린다.
- Skia의 `GrDirectContext::abandonContext()`. GPU API를 부르지 않고 컨텍스트를 버리며, 감싼 텍스처의 release proc은 이때 부른다. Vulkan에서는 Skia가 `VK_ERROR_DEVICE_LOST`를 보면 스스로 컨텍스트를 버린다(`abandoned()`가 참). GL에서는 장치 손실을 보지 못한다. `GrGpu::isDeviceLost()`의 기본값이 거짓이고 `GrGLGpu`가 바꾸지 않는다.

### ANGLE과 libEGL이 손실을 전하는 방법

- ANGLE Vulkan 백엔드: `VK_ERROR_DEVICE_LOST`를 받으면 `DisplayVk::handleError` → `Renderer::notifyDeviceLost` → `DisplayState::notifyDeviceLost`가 디스플레이의 모든 컨텍스트를 잃은 것으로 표시한다(`UnknownContextReset`). 그 뒤 GL 호출은 아무것도 하지 않고, EGL 호출 대부분이 검증 단계(`ValidateDisplay`)에서 `EGL_CONTEXT_LOST`로 실패한다. `eglDestroyContext`와 `eglDestroySurface`도 실패한다. `eglMakeCurrent(NO_CONTEXT)`와 `eglTerminate`만 된다.
- ANGLE은 잃은 Vulkan 디스플레이를 되살리지 못한다(`DisplayVk::restoreLostDevice`가 `EGL_BAD_DISPLAY`). 복구는 컨텍스트를 놓고 `eglTerminate`한 뒤 `eglInitialize`로 새 `Renderer`(새 `VkInstance`와 `VkDevice`)를 만드는 것뿐이다. `eglTerminate`는 현재로 쓰이는 컨텍스트가 있으면 미뤄진다. 손실을 처리할 때 ANGLE은 큐가 비기를 기다리고, 진행 중인 펜스마다 `vkWaitForFences`로 기다린다.
- 반응 알림 전략을 정하지 않은 컨텍스트에서 `glGetGraphicsResetStatus`는 언제나 `GL_NO_ERROR`를 돌려준다. 그래서 GL 쪽 손실은 EGL로 묻는다. `eglQueryContext`가 `EGL_CONTEXT_LOST`로 실패하는지가 가장 싸다.
- ANGLE은 EGL 객체 ID를 지울 때 돌려받아 다시 쓴다(`HandleAllocator`). GL 이름도 컨텍스트마다 처음부터 다시 매긴다. 잃은 디스플레이를 다시 초기화한 뒤에 옛 텍스처의 release proc이 `eglDestroyImageKHR`나 `glDeleteTextures`를 부르면, 새로 만든 같은 번호의 객체를 지울 수 있다. 그래서 옛 컨텍스트가 만든 것은 모두 `eglTerminate` 전에 놓아야 한다.
- libEGL(`frameworks/native/opengl/libs/EGL`)은 `eglInitialize`와 `eglTerminate`를 센다. 드라이버의 디스플레이는 셈이 0이 될 때만 terminate한다. 한 프로세스에서 HWUI 말고도 디스플레이를 쥔 사용자가 있으면(WebView의 GPU 스레드, HWUI의 비트맵 업로더) HWUI의 `eglTerminate`는 드라이버에 닿지 않고, 디스플레이는 모두에게 잃은 채로 남는다. 또 드라이버의 `eglInitialize`가 실패해도 libEGL의 `eglInitialize`는 `EGL_TRUE`를 돌려준다.

### 게스트 드라이버

- ANGLE(`libEGL_angle.so`, `libGLESv2_angle.so`)은 위처럼 손실을 표준 오류로 전하고, 다시 초기화하면 새 장치를 만든다. 고칠 것이 없다.
- gfxstream 게스트 Vulkan 드라이버는 호스트의 결과를 그대로 돌려준다. 비동기 제출(`vkQueueSubmitAsyncGOOGLE`)은 `VK_SUCCESS`를 돌려주고, 손실은 펜스와 대기(`vkWaitForFences`, `vkGetFenceStatus`, `vkGetFenceFdKHR`의 `VK_ERROR_DEVICE_LOST`)에서 드러난다. 표준이 허용하는 동작이다. 드라이버가 스스로 끝내는 곳은 연결에서 응답을 읽지 못할 때(`VulkanStreamGuest::read`의 `abort()`)뿐이다.
- 그래서 장치 손실에서는 게스트 드라이버를 고치지 않는다. 대신 호스트 렌더러가 지켜야 할 것이 있다. R18의 렌더러 패치 0015는 장치 손실을 게스트에 `VK_ERROR_DEVICE_LOST`로 돌려주고 ColorBuffer를 같은 핸들로 다시 만든다. 아래 가운데 나머지는 PC에서 확인한다.
  - 장치를 잃어도 게스트 프로세스마다의 연결을 끊지 않고, 모든 호출에 답한다.
  - 잃은 객체에 대한 호출, 특히 `vkQueueWaitIdle`, `vkWaitForFences`, `vkGetFenceStatus`, `vkAcquireImageANDROID`에는 곧바로 `VK_ERROR_DEVICE_LOST`를 돌려준다. 잃은 작업의 sync fd와 펜스는 신호한다. ANGLE이 손실을 처리하며 이것들을 기다린다.
  - 잃은 객체의 `vkDestroy*`와 `vkFree*`를 받아들인다.
  - 새 장치가 준비되기 전의 `vkCreateInstance`, `vkCreateDevice`에는 오류를 돌려주고, 준비된 뒤에는 새 장치를 준다.
  - gralloc 버퍼(color buffer)의 핸들은 새 장치에서도 다시 가져올 수 있어야 한다. 내용은 잃어도 된다. 게스트가 다시 그린다.
- 스트림이 끝나는 경우는 다르다. 렌더러가 게스트의 치명적 오류로 그 스트림을 끝내거나(R18의 패치 0015), 렌더러 프로세스가 끝나 다시 뜰 때(D25) 게스트 드라이버는 `VulkanStreamGuest::read`에서 프로세스를 끝냈다. 그 프로세스가 SurfaceFlinger면 프레임워크가 다시 시작한다. 그래서 DSA가 빌드하는 게스트 Vulkan 드라이버(`vulkan.ranchu.so`, `dsa/graphics/patches/guest/`)를 고쳤다.
  - 끊긴 스트림은 잃은 것으로 친다(`dsa/graphics/include/dsa_lost_stream.h`). 그 뒤로는 아무것도 보내지 않고, 읽기는 0을 준다(핸들도 개수도 없다). 생성된 인코더는 `VkResult`를 돌려주는 106개 호출 모두에서 결과를 `VK_ERROR_DEVICE_LOST`로 바꾼다. 잃은 스트림은 렌더러가 기다릴 순번(seqno)을 받지 않는다. 쓰기 버퍼를 얻지 못해도 널 포인터로 쓰지 않는다.
  - ANGLE은 이것을 장치 손실로 받아 디스플레이를 잃은 것으로 표시하고, SurfaceFlinger와 HWUI는 위의 패치대로 컨텍스트를 놓고 `eglInitialize`로 새로 만든다. ANGLE은 고치지 않는다.
  - 새로 만들 때의 인스턴스 수준 호출(`vkCreateInstance`와 그 앞의 열거)이 처음으로 오면 프로세스가 새 세대를 시작한다. 다시 뜬 렌더러는 이 프로세스를 모르므로 프로세스를 다시 등록하고(process pipe와 puid, gfxstream의 `renderControlResetProcess`), 순번을 0부터 다시 센다. 스트림을 잃은 스레드는 옛 세대이면 다시 연결한다. 렌더러가 아직 답하지 않으면 잃은 연결을 그대로 두고(호출은 계속 장치 손실) 250 ms마다 다시 시도하며, 인스턴스 만들기는 `VK_ERROR_INITIALIZATION_FAILED`로 실패해 HWUI와 SurfaceFlinger가 나중에 다시 시도한다.
  - GLES 쪽은 ANGLE이 Vulkan 위에서 하므로 gfxstream의 GLES 에뮬레이션 라이브러리(`libEGL_emulation`, `libOpenglSystemCommon.so`의 `HostConnection`)는 이 경로에 없다. 공개 빌드에서 오는 게스트 그래픽 라이브러리는 바꾸지 않으므로 Framework build에 더할 것도 없다.
  - 확인하지 못한 것: 실제로 렌더러를 끝냈다가 다시 띄웠을 때의 동작. 다시 뜬 렌더러가 옛 프로세스의 ColorBuffer를 모르므로 앱 버퍼를 다시 가져오는 일은 렌더러 쪽(D25)이 버퍼를 다시 만들 수 있어야 된다.

### 결정

SurfaceFlinger(RenderEngine 포함), HWUI, libEGL을 고친다. 패치는 `dsa/guest/framework/patches/<project>/<android>/`에 있고 16과 17에 각각 맞췄다. 두 렌더러가 따르는 정책은 하나의 헤더(`DeviceLossRecovery.h`)로 두 패치에 같이 들어가고, 저장소에는 그 원본과 C++ 시험이 있다. 손실은 한 번만 처리하고, 다시 만들기가 실패하면 50 ms에서 1.6 s까지 늘려 가며 다시 시도하고, 포기하지 않는다. 성공할 때마다 리셋 수를 센다.

- `frameworks/native` 0001(RenderEngine): GPU를 쓰기 전마다 손실을 확인한다(Skia의 컨텍스트, GL은 EGL 디스플레이). 손실이면 텍스처와 Skia 컨텍스트를 옛 디스플레이가 있을 때 놓고, EGL 디스플레이를 terminate와 initialize로 다시 만들거나(GL) Vulkan 장치를 새로 만들고(Vk), 컨텍스트를 다시 만들고, SurfaceFlinger가 매핑해 둔 버퍼를 모두 다시 가져온다. 호스트가 아직 답하지 않으면 RenderEngine 스레드에서 10초까지 기다린다. 잃은 장치에서 그린 프레임은 펜스 대신 `DEAD_OBJECT`를 돌려준다. 위 표의 abort는 장치 손실일 때 경고로 바뀐다. `getDeviceResetCount()`와 리스너로 SurfaceFlinger에 알린다.
- `frameworks/native` 0002(SurfaceFlinger): 리셋 수가 바뀌면 각 디스플레이의 client composition 캐시와 planner의 평탄화한 레이어를 버리고 전체를 다시 합성한다. 그리고 `debug.dsa.gpu.resets`를 바꾼다. 같은 GPU에서 다른 프로세스의 장치도 잃었기 때문이다. 이 속성은 SurfaceFlinger가 처음 시작할 때 만든다.
- `frameworks/native` 0003(libEGL): 셈이 0이 아니어도, 드라이버의 디스플레이가 잃은 상태이거나 앞선 초기화가 실패했으면 `eglInitialize`가 드라이버의 디스플레이를 terminate하고 다시 초기화한다. 그 디스플레이에서 만든 객체는 `eglTerminate` 뒤처럼 거절한다.
- `frameworks/base` 0001(HWUI): 창을 그리기 전마다(`CanvasContext::makeCurrent`) 손실을 확인한다. 손실이면 `GrDirectContext`를 버리고 `destroyRenderingContext`로 EGL/Vulkan 컨텍스트와 창들의 surface를 놓는다. 호스트가 답하는지 버리는 컨텍스트로 먼저 시험해 보고 다시 만든다. 기다리는 동안 RenderThread를 막지 않고 다음 시도를 큐에 넣는다. 다시 만들면 창마다 마지막 display list로 전체를 다시 그리고, 다음 동기화에서 UI 스레드에 다시 그리기를 요청한다(`UIRedrawRequired`). `debug.dsa.gpu.resets`가 바뀌어도 같은 일을 한다. 그래서 아무것도 그리지 않던 창도 다시 그려진다. 컨텍스트가 없는 동안 레이어, TextureView 프레임, readback, 하드웨어 비트맵 업로드는 기다리거나 실패로 답하고 앱을 끝내지 않는다.
- GL이나 Vulkan을 직접 쓰는 앱은 ANGLE과 드라이버가 주는 표준 오류(`EGL_CONTEXT_LOST`, `GL_CONTEXT_LOST`, `VK_ERROR_DEVICE_LOST`)를 받는다. 다시 초기화하면 libEGL 0003 덕분에 새 장치를 얻는다.
- 이미지: `make-image.py --framework`가 Framework build의 `surfaceflinger`, `libhwui.so`, `libEGL.so`로 빌드의 파일을 바꾼다. 같은 빌드 번호와 아키텍처에서 만든 것만 받는다. SurfaceFlinger가 `debug_prop`을 쓸 수 있게 하는 규칙을 더하고(`sepolicy/gpu-reset.cil`), `image.json`의 `framework`에 각 파일의 SHA-256을 적는다. `check-image.py`가 이미지 안의 파일과 그 해시를 대조한다.

### 빌드 설계

R16의 번역기 빌드와 같은 방법이다. 받기 스크립트(`fetch-source.py`)와 러너 정리(`free-disk.sh`)는 그대로 함께 쓴다.

- 잠금 파일 `dsa/guest/framework/framework-lock.json`: 버전마다 이미지의 빌드 번호와 대상, 그 빌드의 매니페스트(URL과 SHA-256), 릴리스 설정, Clang과 Rust 판, 받을 프로젝트(16은 99개, 17은 98개). 모든 커밋은 그 빌드의 매니페스트에서 왔다. 워크플로가 매번 매니페스트를 받아 대조한다(`framework_lock.py check-manifest`). 이미지와 같은 소스에서 빌드해야, 바꾼 파일이 이미지에 남는 공개 빌드의 라이브러리와 맞는다.
- 프로젝트 목록은 빌드 파일만 받아 정적으로 찾았다(R16과 같은 방법). `surfaceflinger`, `libhwui`, `libEGL`의 의존을 Android.bp에서 따라가, 정의가 없는 모듈이 이 빌드가 만들지 않는 변형(host musl, Windows, NDK 등)이나 APEX 스텁만 남을 때까지 프로젝트를 더했다. 모두 소스에서 빌드한다. 이미지의 라이브러리를 prebuilt로 링크하지 않는 것은, AIDL과 aconfig가 만든 헤더를 어차피 생성해야 하고 정적 라이브러리(Skia, perfetto, RenderEngine)는 소스가 있어야 하기 때문이다.
  - Skia와 코덱(webp, png, jpeg-turbo, giflib, freetype, harfbuzz, expat, dng_sdk, piex, wuffs, libjxl, libultrahdr, image_io, XMP, brotli, libyuv), Skia의 Rust FFI와 AVIF(`crabbyavif`, `cxx`).
  - perfetto 클라이언트, protobuf와 abseil(호스트 aprotoc와 기기용 lite), boringssl, zstd, lzma, pcre, selinux.
  - `libhwui`가 `android_graphics_jni` → `libmediandk`로 미디어 스택을 끌어온다(`frameworks/av` 39개 폴더, minikin, icu, sonivox, speex, libogg, pdfium).
  - AIDL/HIDL HAL(`hardware/interfaces` 29개 폴더 등), aidl, hidl-gen, sysprop, aconfig와 그 Rust crate 약 96개.
  - Rust 툴체인(16은 1.88.0, 17은 1.93.1)이 기기와 호스트 모두에 필요하다. Java는 빌드하지 않는다.
- 제품: `module_common.mk`와 `core_64_bit_only.mk`에서 시작한 `dsa_framework`. 보드는 Cuttlefish의 `vsoc_x86_64_only`(silvermont)와 `vsoc_arm64_only`(armv8-a, cortex-a53)를 따른다. 세 모듈의 Android.bp는 제품 변수와 Soong config 변수를 읽지 않으므로, 플래그는 빌드 자신의 릴리스 설정이 정한다. `lunch dsa_framework-<release>-userdebug`.
- 빌드(`.github/scripts/framework/build.sh`): 패치를 적용하고(`apply-patches.py`, 정확히 적용되지 않으면 멈춘다) 세 설치 경로만 빌드한다. 환경 변수와 ccache는 번역기 빌드와 같다. 번역기 빌드의 실행에서 배운 대로 unbundled 빌드(`TARGET_BUILD_UNBUNDLED`)로 하고, dexpreopt를 끄고, 이 빌드가 쓰지 않으면서 트리에 없는 프로젝트를 찾는 APEX와 boot jar 모듈의 Android.bp(`bionic/apex`, `art/build`의 `apex`, `boot`, `sdk`, 전원 HAL의 APEX, `libapexsupport`의 시험)를 체크아웃 뒤 지운다(잠금의 `exclude`). ccache는 빌드마다 2 GB까지 GitHub 캐시에 둔다(넷이면 8 GB).
- 워크플로 `framework.yml`("Framework build"): 먼저 잠금과 매니페스트를 대조하고, 패치가 받은 소스에 적용되는지 패치가 바꾸는 폴더만 받아 확인한다. 그다음 Android 판 2개와 아키텍처 2개로 4개 작업을 돌려 `framework-<android>-<arch>-<version>` artifact를 남긴다. `<version>`은 Android 판과 잠금과 패치의 해시다. 이미지 배포는 develop에서 성공한 가장 최근 실행 중 지금의 version을 가진 artifact를 찾아 쓴다.

### 단계별 예상 (실측 전, 작업 하나)

| 단계 | 디스크 | 시간 |
|---|---|---|
| 러너 정리 | 여유 약 45 GB 이상 | 2 ~ 4분 |
| 소스 받기(98 ~ 99개) | 약 9 ~ 12 GB(prebuilt 약 7 ~ 8 GB, 소스 약 1.5 ~ 2.5 GB) | 10 ~ 25분 |
| 패치와 Soong 분석 | `out/` 1 ~ 3 GB, RAM 6 ~ 12 GB | 10 ~ 25분 |
| 컴파일(C/C++ 소스 약 4,600 ~ 4,700개와 perfetto 약 600개, 생성 코드, 호스트 도구, Rust 약 140개 모듈) | `out/` 15 ~ 30 GB, ccache 2 GB | 처음 1.5 ~ 3시간, ccache가 차면 30 ~ 60분 |
| 모으기와 올리기 | 세 파일 약 25 MB | 1분 |

최대 사용량은 약 30 ~ 45 GB다. 처음 실행이 시간을 넘어도 ccache가 남아 다음 실행이 이어 간다.

### 첫 실행(run 42402220551, 네 작업)과 고친 것

네 작업(16과 17, x86_64와 arm64)이 모두 같은 곳에서 멈추었다. 러너 정리, 소스 받기, 패치 적용, 제품 설정, `soong_build` 빌드는 통과했고, Soong이 Android.bp 파일을 읽는 단계가 1초 만에 끝났다. 네 작업의 `error.log`, `soong.log`, `verbose.log.gz`에 있는 오류는 하나다.

```
error: prebuilts/sdk/Android.bp:105:1: module "sdk": no api file found under "prebuilts/sdk"
```

x86_64 작업이 9 ~ 10분 만에 끝난 것도 같은 오류다. 작업 사이의 시간 차이는 소스 받기 시간이다.

- 원인: 잠금이 `prebuilts/sdk`에서 `Android.bp`와 판마다의 `finalized-flags.txt`만 받았다. 그 `Android.bp`의 `sdk`(`prebuilt_apis`)는 파일을 읽는 순간(load hook) 판마다의 API 파일(`<판>/<범위>/api/*.txt`)을 찾고, 하나도 없으면 오류를 낸다(`build/soong/java/prebuilt_apis.go`의 `prebuiltApiFiles`). `ALLOW_MISSING_DEPENDENCIES`는 이것을 덮지 않는다. 이 오류는 파일을 읽는 단계에서 나므로, 그 뒤 단계(의존 해석, 빌드 동작 만들기)의 오류는 이번 로그에 없다.
- 이 프로젝트에서 빌드가 쓰는 것은 확정된 플래그 목록뿐이다. aconfig는 `finalized_flags_record.json`을 소스로 넣고(`build/make/tools/aconfig/aconfig`), 그 genrule이 `//prebuilts/sdk:finalized-api-flags`를 읽는다. `build/soong/aconfig`의 `all_aconfig_declarations`는 `:latest-finalized-flags`를 읽는다. API 파일과 jar는 이 빌드가 쓰지 않는다.
- 고친 것: API 파일을 받는 대신 `Android.bp`를 받지 않고, `build.sh`가 그 자리에 대역(`.github/scripts/framework/stand-ins/common/prebuilts/sdk/Android.bp`)을 둔다. 대역에는 원래 파일의 라이선스와 `finalized-api-flags`, `latest-finalized-flags`가 그대로 있고 `prebuilt_apis`는 없다. 잠금은 `/NOTICE`와 `/*/finalized-flags.txt`를 받는다. 체크아웃에 진짜 `Android.bp`가 있으면 `build.sh`가 멈춘다.
- 이것으로 분석이 멈추지 않는다고 보는 근거: 번역기 트리에는 `prebuilts/sdk`가 아예 없었는데 분석이 통과했다(R16의 여섯 번째 실행). Java 모듈의 SDK 의존은 SDK jar가 없으면 `ALLOW_MISSING_DEPENDENCIES`에서 누락으로 처리된다(`build/soong/java/sdk.go`의 `toPrebuilt`).
- 앞을 내다보고 고친 것: R16의 일곱 번째 실행처럼 Soong은 모든 `python_binary_host`에 `libsqlite`와 `aprotoc`를 붙인다. 이 빌드도 `gensyscalls` 같은 Python 도구로 `libc`의 소스를 만든다. 트리에 `libsqlite`가 없으므로(`toolchain/pgo-profiles`의 같은 이름은 자기 namespace 안의 `fdo_profile`이다) 제품 폴더에 빈 호스트 `libsqlite`를 둔다(`device/hosttools`). `aprotoc`는 대역을 두지 않는다. 트리에 `external/protobuf`가 있어 진짜가 정의되어 있고, 대역은 이름이 겹친다. SurfaceFlinger와 perfetto의 `.proto` 소스가 어차피 진짜를 쓴다.

번역기 트리에 없던 모듈 형식도 살폈다. 잠금과 같은 부분 체크아웃 규칙으로 빌드 파일만 받은 사본에서, 트리의 Android.bp(16은 2,264개, 17은 2,684개)가 쓰는 모듈 형식을 번역기 트리의 것과 비교했다. 새로 있는 위험한 형식은 `frameworks/base`의 Java와 API 모듈이다(`combined_apis`, `java_sdk_library`, `system_java_library`, `non_updatable_exportable_droidstubs`, `java_genrule_combiner`, `platform_compat_config`, `*_mock_init`). 이번 실행에서 파일을 읽는 단계는 이것들 모두 통과했다. 그 뒤 단계는 아직 돌지 않았다.

- `libs/hwui`와 `core/java`의 Android.bp는 빌드에 필요한 모듈(`libhwui`, `libpowermanager_aidl` 등)을 함께 가지므로 뺄 수 없다.
- `services`, `services/core`, `api`의 Android.bp는 빌드에 필요한 모듈이 없다. 하지만 빌드에 필요한 파일(`services/backup`, `display/feature`의 aconfig 선언)과 `frameworks/base/Android.bp`가 그 모듈 이름을 쓰고, `api`에는 Soong 플러그인(`soong-api`)이 있다. 지우면 누락이 더 늘 뿐이라 두었다.
- 빌드가 쓰지 않는 Java 전용 시험 파일(`frameworks/native/opengl/tests`의 앱 7개, `libnativehelper/tests_mts`, `system/tools/hidl/test`의 Java 시험 3개)은 번역기 트리에도 있던 형식(`android_app`, `android_test`, `java_library`)뿐이라 두었다.

다음 실행에서 분석의 뒤 단계가 처음으로 돈다. 그 단계의 오류(패닉이나 의존 누락이 아닌 검사)는 그 로그에서 고친다.

### 두 번째 실행(run 42405466107, 네 작업)과 고친 것

`prebuilts/sdk` 대역으로 파일 읽기를 지나, 네 작업 모두 분석이 끝까지 돌았다(`[100% 248/248]`). 그 뒤 의존 누락으로 넘어가지 않는 오류에서 멈추었다. 패닉은 없었다. 네 작업의 `error.log`와 `verbose.log.gz`에 있는 서로 다른 오류는 둘이다.

| 오류 | 작업 |
|---|---|
| `frameworks/base/core/java/Android.bp:635:9: module "android.security.rkp_aidl_interface": srcs: No sources provided in` | 16 arm64, 16 x86_64 |
| `external/libultrahdr/Android.bp:15:1: "//external/libultrahdr" depends on undefined module "adobe_hdr_gain_map_license"` | 16 x86_64, 17 arm64, 17 x86_64 |

- `aidl_interface`는 `srcs`가 가리키는 파일이 하나도 없으면 오류를 낸다(`system/tools/aidl/build/aidl_interface.go`의 `getPaths`). 이름만 적은 파일은 `ALLOW_MISSING_DEPENDENCIES`에서 경로가 남아 지나가지만, 글로브(`android/security/rkp/*.aidl`)는 맞는 파일이 없으면 빈 목록이 된다. `core/java/Android.bp`는 `core/java/android/os` 등이 cone에 있어 조상 폴더의 파일로 들어오는데, 그 안의 다른 모듈의 소스 폴더는 받지 않았다.
- 패키지의 기본 라이선스(`default_applicable_licenses`)는 모듈의 의존과 달리 없으면 오류다. `adobe_hdr_gain_map_license`는 `external/libultrahdr/adobe-hdr-gain-map-license/Android.bp`에 있는데, cone이 `lib/include`와 `lib/src`만 받았다.
- 16 arm64에 라이선스 오류가 없고 17에 rkp 오류가 없는 것은 오류가 없어서가 아니다. 두 판 모두 같은 모듈과 같은 cone을 가지므로 둘 다 고친다.

빌드 파일만 받은 사본에서 같은 종류를 모두 찾았다(트리의 모든 Android.bp를 문자열과 주석을 구분해 읽었다).

- 트리에 없는 라이선스: `adobe_hdr_gain_map_license` 하나뿐이다. `license_kinds`와 `team`(`default_team`)은 모두 트리에 있다.
- 받은 폴더 밖을 가리키는 글로브만 `srcs`로 가진 모듈 중 빈 목록이 오류인 것은 `aidl_interface` 셋이다. `android.security.rkp_aidl`(16, 17), `frameworks/base/media`의 `soundtrigger_middleware-aidl`(16, 17)과 `audio-aidl`(17). 나머지(`filegroup`, Java 모듈, `cc_defaults`)는 빈 글로브를 오류로 치지 않는다. 미디어의 둘은 이번 로그에 없었다. 같은 형식이고 폴더가 작아(`.aidl` 4 ~ 12개) 함께 받는다.

고친 것: `frameworks/base`의 cone에 `core/java/android/security/rkp`, `media/aidl/android/media/soundtrigger_middleware`, (17은) `media/aidl/android/media/audio`를 더했고, `external/libultrahdr`의 cone에 `adobe-hdr-gain-map-license`를 더했다. 새 폴더와 그 조상 폴더에는 다른 Android.bp가 없다. 받을 소스가 작으므로 `core/java/Android.bp`를 대역으로 나누지 않았다. 대역은 그 파일이 바뀔 때마다 따로 맞춰야 한다. (세 번째 실행 뒤 이 판단을 바꾸었다. 아래 절을 본다.)

### 세 번째 실행(run 42407626974, 네 작업)과 고친 것

같은 종류의 오류가 한 번에 하나씩 드러나 실행마다 하나씩 고치게 되었으므로, 이번에는 트리 전체를 정적으로 훑었다. 네 작업의 오류는 다음과 같다.

| 오류 | 작업 |
|---|---|
| `frameworks/av/services/mediaresourcemanager/Android.bp:50:9: module "resourceobserver_aidl_interface_interface": srcs: No sources for a previous version in aidl_api/resourceobserver_aidl_interface/1` (그리고 같은 모듈의 `No sources provided in aidl_api/resourceobserver_aidl_interface/1`) | 16 arm64, 16 x86_64, 17 arm64 |
| `internal error: panic in GenerateBuildActions for module "framework-javastream-protos"` (`runtime error: index out of range [0] with length 0`, `build/soong/genrule/genrule.go`의 `AndroidMk`) | 17 x86_64 |

- 첫째: 고정된(frozen) 판이 있는 `aidl_interface`는 판마다 `aidl_api/<이름>/<판>`의 파일을 소스로 쓰고, 그 폴더가 비면 오류다. cone이 `services/mediaresourcemanager/aidl`만 받았다.
- 둘째: `gensrcs`는 입력 파일마다 출력을 하나 만든다. 입력이 하나도 없으면 출력 목록이 비고, `AndroidMk`가 그 첫 원소를 읽다 패닉한다. `framework-javastream-protos`(`frameworks/base/ProtoLibraries.bp`, 루트 `Android.bp`의 `build = [...]`로 읽힘)의 입력은 받지 않은 `core/proto/**/*.proto` 글로브와 트리에 없는 모듈이다. 패닉은 분석 전체를 끝내므로, 다른 세 작업은 그보다 앞의 오류에서 멈추었을 뿐 같은 패닉을 안고 있다.

방법: 잠금의 프로젝트마다 고정한 커밋의 파일 목록을 받았다(`git fetch --depth=1 --filter=blob:none`, 블롭 없이 트리만, 187개 커밋). 잠금의 cone, patterns, `exclude`로 체크아웃을 흉내 내고(16은 약 14만, 17은 약 16만 파일), 그 안의 빌드 파일(`build = [...]`로 읽히는 `.bp` 포함, 16은 2,265개, 17은 2,685개)을 문자열과 주석을 구분해 읽어 다음을 확인했다.

1. `aidl_interface`(16과 17 모두 약 100개): 글로브만으로 된 `srcs`가 받은 파일과 맞는가, `srcs`가 모듈이면 그 모듈이 파일을 내는가, `versions`와 `versions_with_info`의 판마다 `aidl_api/<이름>/<판>`에 `.aidl`이 있는가(48개 판), 고정 판이 있는 안정 인터페이스에 `aidl_api/<이름>/current`가 있는가, `imports`의 모듈이 트리에 있는가. 이름을 적은 파일은 없어도 `ALLOW_MISSING_DEPENDENCIES`에서 경로가 남으므로(`build/soong/android/paths.go`의 `expandOneSrcPath`가 오류를 의존 누락으로 바꾼다) 빈 목록이 되지 않는다.
2. `gensrcs`: `srcs`가 파일 하나라도 내는가. `:모듈`은 그 모듈이 파일을 내는지 따라간다.
3. 패키지의 기본 라이선스가 트리에 있는가.

찾은 것은 위의 둘과, 같은 패닉을 낼 `framework-cppstream-protos`다(`jemalloc`의 시험용 `gensrcs` 넷은 이름을 적은 파일이라 해당하지 않는다). 다른 판 폴더, 가져오는 모듈, 라이선스는 모두 맞았다.

고친 것:

- `frameworks/av`의 cone에 `services/mediaresourcemanager/aidl_api`를 더했다.
- `frameworks/base`의 Java 쪽 빌드 파일은 대역으로 바꾼다. 소스를 하나씩 더 받는 대신, 네이티브 빌드가 쓰지 않는 Java, API, proto 모듈을 트리에서 뺀다. 잠금이 `frameworks/base`의 `Android.bp`, `api/Android.bp`, `core/java/Android.bp`, `libs/hwui/Android.bp`, `media/Android.bp`, `services/Android.bp`, `services/core/Android.bp`를 `exclude`로 지우고, `build.sh`가 `.github/scripts/framework/stand-ins/`(판에 공통인 `common/`과 판마다의 `android16/`, `android17/`)의 대역을 그 자리에 둔다. 체크아웃에 대역의 자리에 해당하는 파일이 있으면 `build.sh`가 멈춘다. `prebuilts/sdk`의 대역도 이 폴더로 옮겼다.
  - 루트 `Android.bp`: 패키지와 라이선스(`frameworks_base_license`, `frameworks/base`의 여러 빌드 파일이 기본 라이선스로 쓴다)만 두고, `build = [...]`에는 `AconfigFlags.bp`만 남긴다. `ProtoLibraries.bp`, `TestProtoLibraries.bp`, `Ravenwood.bp`는 읽히지 않는다. 세 라이브러리가 닿는 모듈은 원래 파일에 없다.
  - `core/java/Android.bp`: 패키지, 세 라이브러리가 닿는 모듈(`libpowermanager_aidl`, `android.os.hintmanager_aidl`, `process_info_service_aidl` 등), 트리의 다른 빌드 파일이 이름으로 부르는 Java가 아닌 모듈(`frameworks/native/libs/binder`의 `activity_manager_*_aidl` 파일 그룹, `hardware/interfaces/media/c2`가 헤더로 쓰는 `HardwareBuffer_aidl`, `libincremental_aidl-cpp` 등), 그리고 그것들이 다시 부르는 같은 파일의 모듈만 둔다. 패키지를 포함해 16은 68개 블록 중 13개, 17은 84개 중 15개다.
  - `libs/hwui/Android.bp`: 원래 파일에서 Java 라이브러리 `framework-graphics`(`java_sdk_library`)와 그 파일 그룹 둘만 뺐다. 그 모듈은 prebuilt SDK와 프레임워크 소스로 API 스텁을 만드는데, 둘 다 트리에 없다. `libhwui`를 포함한 나머지 블록은 그대로다.
  - `api`, `media`, `services`, `services/core`의 `Android.bp`는 대역 없이 뺀다. 세 라이브러리가 닿는 모듈이 없다. 거기서 정의한 기본값(`platform_service_defaults` 등)을 남은 파일이 쓰지만, 없는 기본값은 의존 누락으로 처리된다. 지금 트리에도 없는 기본값을 부르는 곳이 41곳 있는데 세 번째 실행의 의존 해석 단계가 통과했다. `api/Android.bp`의 `soong_config_module_type`을 가져오는 파일은 `api/StubLibraries.bp`와 `services/Android.bp`뿐이고, 둘 다 함께 빠진다.
  - 대역의 블록은 고정한 커밋의 파일에서 그대로 잘라 냈다. 판마다 다른 파일은 판마다 따로 둔다.
- 앞의 실행에서 더한 `core/java/android/security/rkp`와 미디어의 AIDL 폴더 둘은 그 모듈이 대역에서 빠지므로 cone에서 다시 뺐다.

대역을 둔 뒤 같은 훑기를 다시 돌려, 위의 세 항목에서 걸리는 것이 없음을 확인했다. 대역 뒤 트리에 남은 모듈 형식 중 번역기 트리에 없던 Java 쪽 형식은 `external/perfetto`의 `android_library`와 시험 도우미 앱(`android_test_helper_app`)뿐이다.

이 훑기가 보지 못하는 것: 빌드 동작을 만드는 단계(`GenerateBuildActions`)는 어느 실행에서도 끝까지 돈 적이 없다. 그 단계의 다른 모듈 형식의 검사나 패닉은 다음 실행이 처음으로 보인다.

### 네 번째 실행(run 42410302598, 네 작업)과 고친 것

Soong 분석이 네 작업 모두 끝까지 돌았다(`GenerateBuildActions` 포함). 그다음 ninja가 컴파일을 하나도 시작하기 전에 멈추었다.

```
ninja: 'out/host/linux-x86/bin/protoc-gen-rust', needed by 'out/soong/.intermediates/build/make/tools/aconfig/aconfig_protos/libaconfig_rust_proto/linux_glibc_x86_64_source/aconfig.rs', missing and no known rule to make it
```

17의 두 작업은 같은 자리에서 `protoc-gen-rs`다. aconfig의 Rust protobuf 코드를 만드는 플러그인이다. Soong은 이 플러그인을 의존이 아니라 설치 경로로 부른다(`build/soong/rust/protobuf.go`의 `HostToolPath(ctx, "protoc-gen-rs")`). 그래서 트리에 그 모듈이 없어도 의존 누락이 되지 않고, ninja에서 만들 규칙이 없는 파일이 된다. ninja는 이런 파일 하나에서 멈추므로 같은 종류의 다른 파일은 로그에 없다. 그래서 같은 종류를 모두 정적으로 찾았다.

1. 경로로 부르는 호스트 도구: 체크아웃 안의 Go 코드에서 `HostToolPath`, `HostBinToolVariable`, `HostJavaToolPath` 등이 부르는 도구 이름을 모두 모아, 트리에 그 이름의 모듈이 없는 것을 골랐다. Java, APEX, 파일 시스템, dexpreopt에만 쓰이는 것(`d8`, `r8`, `metalava`, `apexer`, `avbtool` 등)을 빼면 16은 `protoc-gen-rust`, `grpc_rust_plugin`, `llvm-rs-cc`, `stg`, `stgdiff`, `xmllint`, `assemble_vintf`, `sqlite3_noicu`, 17은 `protoc-gen-rs`, `grpc_rust_plugin`, `llvm-rs-cc`, `stg`, `stgdiff`, `sqlite3_noicu`가 남는다. 이 빌드가 쓰는 것은 protobuf 플러그인뿐이다. `grpc_rust_plugin`은 gRPC proto에만, `llvm-rs-cc`는 RenderScript에만, `stg`와 `stgdiff`는 NDK ABI 검사에만, `sqlite3_noicu`는 compliance 데이터베이스에만 쓰인다. 묶음 안에 `prebuilt_etc_xml`도 `vintf_fragment`도 없으므로 `xmllint`와 `assemble_vintf`도 쓰이지 않는다. ninja는 요청한 대상에 이르는 동작의 입력만 확인하므로 쓰이지 않는 도구는 문제가 되지 않는다.
2. 경로로 부르는 소스 파일: 같은 Go 코드에서 `prebuilts/...` 같은 소스 경로 문자열을 모두 모아, 체크아웃에 없는 것을 골랐다. 이 빌드가 쓰는 것은 `prebuilts/misc/linux-x86/yasm/yasm` 하나다. cc는 `.asm` 소스를 yasm으로 어셈블하고(`build/soong/cc/builder.go`의 `yasmCmd`, `CommandDeps`), `libjpeg-turbo`가 x86_64에서 `.asm` SIMD 소스를 쓴다. 나머지(`prebuilts/clang-tools`의 ABI 도구, `prebuilts/abi-dumps`, `toolchain/pgo-profiles/orderfiles` 등)는 `SKIP_ABI_CHECKS`나 이 묶음이 쓰지 않는 기능에만 걸린다.
3. Soong이 이름으로 붙이는 의존(`AddHostToolDependencies`, `AddFarVariationDependencies` 등의 문자열): cc, Rust, Python, genrule, aconfig, aidl, hidl, sysprop에서 트리에 없는 이름은 변형 이름(`link`, `static`, `rlib`)뿐이었다.

플러그인이 닿는 모듈을 Android.bp에서 따라가(41개), 트리에 없는 것의 폴더를 골랐다. `external/rust/android-crates-io`의 `protobuf-codegen`, `protobuf-parse`, `tempfile`, `fastrand`, `rustix`, `errno`, `which`(17은 `getrandom`도)다.

고친 것: 잠금이 그 상자 폴더들과 `prebuilts/misc`(cone `linux-x86/yasm`, 커밋은 두 빌드의 매니페스트에서)를 받는다. 바꾼 잠금으로 세 번째 실행 뒤의 훑기와 위의 확인을 다시 돌려 걸리는 것이 없음을 보았다. `prebuilts/misc`의 루트 `Android.bp`는 라이선스와 `dirgroup` 하나뿐이다.

남은 것: 의존으로 붙는 모듈이 트리에 없으면 ninja가 아니라 그 동작이 실행될 때 "missing dependencies"로 실패한다. 그런 것은 다음 실행의 `m -k` 요약에 모두 나온다.

### 다섯 번째 실행(run 42412209469, 네 작업)과 빌드 전 입력 확인

네 작업 모두 ninja가 컴파일 전에 멈추었다.

```
ninja: 'external/boringssl/src/util/fipstools/inject_hash/inject_hash.go', needed by 'out/host/linux-x86/bin/go/bssl_inject_hash/linux_glibc_x86_64/pkg/bssl_inject_hash.a', missing and no known rule to make it
```

`bssl_inject_hash`는 boringssl의 FIPS 해시를 넣는 Go 도구(`blueprint_go_binary`)다. Go 모듈의 소스는 `ALLOW_MISSING_DEPENDENCIES`가 덮지 않는다. 앞의 정적 훑기는 AIDL, gensrcs, 라이선스, 경로로 부르는 도구를 보았지만 Go 모듈의 소스는 보지 않았다. 이렇게 훑기가 종류 하나를 놓칠 때마다, ninja가 첫 파일에서 멈추므로 실행 한 번에 파일 하나씩만 드러났다.

그래서 빌드 자체가 빠진 입력을 한꺼번에 보이게 했다. 두 빌드 스크립트(`framework/build.sh`, `translator/build.sh`)가 이제 세 단계로 돈다.

1. `m --skip-ninja <대상>`: Soong 분석만 하고 ninja 파일(`out/combined-<제품>.ninja`)을 만든다.
2. `check-ninja-inputs.py`(`.github/scripts/translator/`, 두 빌드가 함께 쓴다): `ninja -t graph <대상>`으로 대상에 이르는 그래프 전체를 받는다. 어떤 동작도 만들지 않는 파일(그래프의 잎)을 모두 골라, 그중 트리에 없는 것을 모두 적고 실패한다. 목록은 콘솔과 `out/missing-inputs.txt`(로그 artifact에 포함)에 남는다. `-t graph`는 모든 ninja 판에 있다.
3. 둘 다 통과하면 `m -k`로 진짜 빌드를 한다.

이 확인은 ninja가 멈추는 바로 그 조건(입력이 없고 만드는 규칙도 없음)을 대상의 그래프 전체에 대해 한 번에 본다. 그래서 다음 실행은 이런 파일을 종류와 상관없이 모두 한 목록으로 보인다. 의존 누락으로 처리된 모듈은 Soong이 오류 동작으로 바꾸므로 이 목록이 아니라 `m -k`의 요약에 나온다.

이번 오류도 고쳤다. 정적 훑기에 Go 모듈(`bootstrap_go_package`, `blueprint_go_binary`)의 `srcs`를 더해 보니, 트리에 없는 것은 boringssl의 셋(`bssl_inject_hash`, 그것이 쓰는 `bssl_ar`, `bssl_fipscommon`)이었다. boringssl의 cone에 `src/util/ar`, `src/util/fipstools/fipscommon`, `src/util/fipstools/inject_hash`를 더했다. `src/util`과 `src/util/fipstools` 바로 아래의 파일(Go 스크립트 등)은 조상 폴더라 함께 들어오고, 그 안에 Android.bp는 없다.

### 여섯 번째 실행(run 42413908578, 네 작업)과 고친 것

빌드 전 입력 확인은 네 작업 모두 통과했다(16은 약 9,700개, 17은 약 26,000개 입력, 빠진 것 0). 그 확인은 번역기 빌드(run 42413953470, 두 판)에서도 통과했다. 16의 두 작업은 처음으로 컴파일까지 갔다. 17의 두 작업은 ninja가 실행 중에 멈추었다.

```
ninja: 'frameworks/base/core/jni/android_media_MediaMetricsJNI.h', needed by '.../libandroid_text_flags_rust.rlib', missing and no known rule to make it
```

이 파일은 `media/jni`의 같은 이름을 가리키는 링크인데, cone이 `media/jni`를 받지 않았다. 입력 확인이 링크 자체가 있는지만 보아(`lexists`) 놓쳤다. 이제 링크를 따라가 대상이 있는지 본다(`exists`).

16의 컴파일 오류는 `error.log`에서 모았다(두 작업이 같다).

| 오류 | 개수(작업 하나) | 원인 |
|---|---|---|
| `'build/build_config.h' file not found` | 409 | pdfium의 `build` 폴더를 받지 않음 |
| `'build/XMP_BuildInfo.h' file not found` | 31 | XMP의 `build` 폴더를 받지 않음 |
| `'native_bridge_support/weak_for_native_bridge.h'` (`-include`) | 24 | arm-optimized-routines의 그 폴더를 받지 않음 |
| `src/__support/macros/config.h`, `hdr/types/*.h`, `include/llvm-libc-types/*.h` | 50여 | llvm-libc의 `src` 일부, `hdr`, `include`만 받음 |
| `utils/Errors.h`, `utils/FastStrcmp.h`, `utils/KeyedVector.h`, `utils/Log.h` | 20여 | Soong의 전역 include 폴더 `system/core/include`(`libutils/include`로 가는 링크)를 받지 않음 |
| `contrib/optimizations/insert_string.h` | 2 | zlib의 루트 파일만 받음 |
| `../include/cxx.h`, Rust `file not found for module syntax`와 그 뒤의 Rust 오류 | 50여 | cxx의 `include`, `syntax`, `gen/src`를 받지 않음(`gen/cmd/src/syntax` 등은 그 폴더로 가는 링크) |
| `missing dependencies: module source path "external/rust/crabbyavif/include" does not exist` | 1 | crabbyavif의 `include` |
| `missing dependencies: stats-log-api-gen` | 4 | SurfaceFlinger와 libhwui의 statslog 소스를 만드는 도구가 `frameworks/proto_logging`에 있는데 그 프로젝트가 없음 |

statslog 생성 헤더를 기다리는 SurfaceFlinger와 libhwui의 소스는 컴파일되지 않았다. 그래서 그 다음 층은 이 로그에 없다.

같은 종류를 한 번에 찾으려고 세 가지를 확인했다.

1. 링크: 고정한 커밋마다 트리의 링크(모드 120000)와 그 대상을 모두 받아(4,503개), 체크아웃 안의 링크 중 대상이 체크아웃 밖인 것을 골랐다. 빌드에 걸리는 것은 `system/core/include`와 `hardware/libhardware/include`(전역 include 폴더, 폴더째 받지 않음), `frameworks/av/include/media`의 셋(`libaudioprocessing`, `libeffects/factory`, `libmediaplayerservice`의 `include`로), `frameworks/base/core/jni`의 둘(`media/jni`로), cxx의 셋이다. 나머지는 Linux arm64와 macOS 도구, 시험 자료, 문서다.
2. include: 오류가 난 프로젝트들(pdfium, XMP, llvm-libc, zlib, cxx, arm-optimized-routines, crabbyavif)을 새 cone으로 실제로 받아(두 판 합쳐 약 330 MB, `.scratch`), 모든 C/C++ 파일의 `#include`가 같은 프로젝트의 파일을 가리키는데 그 파일이 체크아웃에 없는 경우를 찾았다. 남은 것은 pdfium의 시험(`testing/`)과 끈 XFA(`xfa/`, `fxbarcode/`)뿐이다.
3. 도구: `stats-log-api-gen`이 닿는 모듈을 따라가니(16은 285개, 17은 293개) `frameworks/proto_logging` 말고는 모두 트리에 있었다.

고친 것:

- 통째로 받는다(작은 프로젝트): zlib, cxx, arm-optimized-routines, crabbyavif, 그리고 새로 더하는 `frameworks/proto_logging`(커밋은 두 빌드의 매니페스트에서).
- cone에 더한다: pdfium `build`, `core`(전체), XMP `XMPCommon`, `XMPCore`, `XMPFiles`, `XMPFilesPlugins`, `build`, `third-party`(`docs`, `samples`, `tools`만 빼고), llvm-libc `hdr`, `include`, `libc`, `src`(전체), `system/core`의 `include`, `hardware/libhardware`의 `include`, `frameworks/av`의 세 `include` 폴더, `frameworks/base`의 `media/jni`.
- `frameworks/base/media/jni`의 Android.bp 넷은 `exclude`로 지운다. 받는 이유는 링크 대상 두 파일뿐이고, 그 모듈은 빌드가 쓰지 않는다.

바꾼 잠금으로 앞의 훑기(AIDL, gensrcs, 라이선스, Go 소스, 경로로 부르는 도구, 링크)를 다시 돌려 걸리는 것이 없음을 보았다. 남은 링크는 XMP의 macOS 플러그인 묶음(빌드하지 않음)과 위의 도구, 시험 자료뿐이다.

남은 것: SurfaceFlinger, libhwui, libEGL과 그 아래 라이브러리(frameworks/native, frameworks/base, Skia 등)의 소스는 이번에 받아 보지 않았다. 컴파일이 처음으로 그 소스까지 가므로 그 헤더는 다음 실행이 보인다. `m -k`라 서로 기다리지 않는 소스는 모두 시도되므로, 그 층의 빠진 헤더는 한 실행에 함께 나온다.

### 일곱 번째 실행(run 42417890599, 네 작업)과 고친 것

프레임워크 쪽이 네 작업 모두 컴파일되었다. 실패한 동작은 16이 작업마다 819개, 17이 65개였다. `error.log`의 `FAILED` 블록마다 첫 오류를 모아 원인으로 묶었다.

| 원인 | 16 | 17 |
|---|---|---|
| Skia `include/sksl/SkSLVersion.h`, `SkSLDebugTrace.h`(Skia, libhwui, RenderEngine) | 813 | 0 |
| `upb/port/def.inc`(protobuf의 upb) | 0 | 59 |
| `hardware_legacy/power.h`(PowerHalLoader) | 1 | 1 |
| Rust bindgen 동작마다 `Unable to find libclang` | 5 | 5 |

17에는 Skia 오류가 없었다. 17의 Skia도 같은 cone이고 같은 헤더를 읽으므로(아래 확인), 그 소스가 이번에 컴파일 단계까지 가지 않았던 것으로 보고 두 판 모두 고친다.

- bindgen: Soong은 bindgen을 C 컴파일러와 다른 Clang으로 돌린다(`build/soong/rust/bindgen.go`의 `bindgenClangVersion`, 16은 `clang-r563880`, 17은 `clang-r584948`). `LIBCLANG_PATH`가 그 판의 `lib/`인데, 잠금은 C 컴파일러의 Clang(`{clang}`)만 받았다. 그 판의 `bin`과 `lib`를 더 받는다(판마다 약 2 GB).
- `hardware_legacy/power.h`: Soong의 전역 include 폴더 `hardware/libhardware_legacy/include`의 파일인데 그 프로젝트가 잠금에 없었다. 파일 31개짜리 프로젝트라 통째로 받는다(커밋은 매니페스트에서).

같은 층을 한 번에 찾으려고, 이번에는 잠금의 소스 프로젝트를 모두 잠금의 cone 그대로 실제로 받았다(프리빌트, `build/`, Rust 상자, Go 프로젝트는 빼고 판마다 약 1.6 GB, `.scratch`). 그리고 세 라이브러리가 닿는 모듈(묶음의 모듈 목록과, 그 뒤 더한 호스트 도구 `stats-log-api-gen`, `aprotoc`, `aconfig`, `hidl-gen`, `aidl` 등에서 따라간 모듈)의 C, C++, 어셈블리 소스를 시작점으로 `#include`를 끝까지 따라갔다(16은 소스 5,201개에서 파일 12,769개, 17은 5,324개에서 13,105개). 찾는 곳은 소스의 폴더, 그 모듈들이 적은 include 폴더, Soong의 전역 include 폴더다. 체크아웃에 없고 고정한 커밋에는 있는 헤더가 다음이었다.

- Skia `include/sksl`, `include/third_party`(16, 17).
- protobuf `upb/port`, `rust/cpp_kernel`(17, protoc의 Rust 생성기가 쓴다).
- 그 밖에는 조건부로만 읽히는 것뿐이다. pdfium의 시험과 끈 XFA, googletest의 abseil flags(`GTEST_HAS_ABSL`), Skia의 Rust BMP와 Vello가 그렇다.

어느 프로젝트에도 없는 include 경로도 살폈다. 생성되는 헤더(AIDL, perfetto의 `pbzero`), 다른 플랫폼의 헤더, Clang 자신의 헤더(`sanitizer/*`)뿐이었다. 빠진 프로젝트는 `hardware/libhardware_legacy` 하나다.

고친 것: 위의 cone 넷과 Clang 두 폴더, 새 프로젝트 하나를 잠금에 더했다. 바꾼 잠금으로 받아 같은 확인을 다시 돌렸고, 남은 것은 조건부 헤더뿐이었다. 앞의 훑기(AIDL, gensrcs, 라이선스, Go 소스, 링크)도 깨끗했다.

남은 것: 이 확인은 `#if`를 보지 않으므로, 실제로는 읽히지 않는 헤더를 보고할 수는 있어도 읽히는 헤더를 놓치지는 않는다. 다만 생성되는 소스(AIDL, protobuf, 생성 헤더)의 include는 따라가지 못한다. 그리고 묶음의 모듈 목록이 정적 추정이라 거기 없는 모듈의 소스는 시작점이 아니다. 링크는 아직 한 번도 돌지 않았으므로 링크 오류(빠진 정적 라이브러리, 심볼)는 다음 실행이 처음 보인다.

### 남은 위험과 확인할 것

- 프로젝트 목록은 읽어서 만든 추정이다. 첫 실행의 `m -k` 요약이 빠진 프로젝트를 보이고, `update-lock.py --add`로 더한다. kati가 `bp4a`, `cp2a`와 이 제품을 받아들이는지, Soong만 쓰는 빌드에서 설치 경로를 대상으로 줄 수 있는지는 번역기 빌드와 같이 실행에서 확인한다.
- 패치는 받은 소스에 정확히 적용됨을 확인했지만 컴파일하지 않았다. 컴파일 오류는 첫 실행에서 고친다.
- 실제 복구는 PC에서 본다(NFR-5의 수동 확인). 앱 창 여러 개를 연 채 `dxcap -forcetdr`로 GPU를 리셋하고, 몇 초 안에 모든 창이 다시 그려지는지, system_server의 pid가 그대로인지, 스크롤 위치와 입력한 글자가 남는지 본다. 결과는 이 절에 적는다.
- 아무 프로세스도 GPU를 쓰지 않는 동안 리셋되면, 처음으로 그리는 프로세스가 손실을 만날 때까지 아무도 알지 못한다. 그 프로세스가 SurfaceFlinger면 모든 앱에 알린다. 앱이면 그 앱만 다시 그리고, SurfaceFlinger는 그 앱의 새 버퍼를 가져오다 손실을 만난다. 완전히 멈춘 화면에서 곧바로 다시 그리려면 호스트가 리셋을 게스트에 알려야 한다(예: 제어 채널의 알림을 받아 `debug.dsa.gpu.resets`를 바꾸는 시스템 쪽 처리). 제어 프로토콜과 정책을 바꾸는 일이라 소유자에게 따로 제안한다.
- WebView(Chromium)는 자기 GPU 스레드의 컨텍스트 손실을 스스로 처리한다. 그 동작은 확인하지 않았다.
- `SkiaVulkanPipeline`과 하드웨어 비트맵 업로더의 Vulkan 경로는 이 이미지들에서 쓰이지 않는다. 업로더의 Vulkan 경로는 `GrDirectContext`를 만들지 못하면 여전히 끝낸다.
- 보호 콘텐츠(protected) 컨텍스트는 다시 만들지만, 보호 버퍼의 다시 가져오기는 시험하지 않았다(DSA에 DRM 경로가 없다).

## R20. 내비게이션 막대 없는 창의 목록 아래 여백: 코드가 패딩을 정하는 곳과 다시 컴파일할 앱 (FR-22, FR-5)

### 확인한 자료
- Android 16 이미지 입력(빌드 15581820, BP4A.251205.006)의 `Settings.apk`, `DocumentsUI.apk`, `com.android.permission.capex` 안의 `PermissionController.apk`. Android 17(빌드 16373615)의 `Settings.apk`, `DocumentsUI.apk`. 모두 `.scratch/list-bottom-margin/`에 있다.
- AOSP 소스의 `TopLevelSettings.java`, `DirectoryFragment.java`, idmap2의 `ResourceMapping.cpp`, otatools의 `releasetools/apex_utils.py`와 `sign_target_files_apks.py`.
- Android 16 target_files의 `oat/` 목록과 각 odex의 oat 헤더. Release image 42329110634(2026-11-07)의 빌드 로그.

### 방법
- 레이아웃은 SDK build-tools 37.0.0의 `aapt2 dump xmltree`로 읽었다. dex는 `make-image.py`의 `dex_methods`와 `dex_instructions`로 메서드를 풀어 보았다. 고친 APK도 같은 방법으로 다시 읽어 결과를 확인했다.

### 찾은 것
- **Settings 홈(16)**: `TopLevelSettings.onCreateRecyclerView`가 `setPadding(mPaddingHorizontal, 0, mPaddingHorizontal, 0)`을 부른다. 16 빌드에는 `setPaddingHorizontal`이 없고(두 창 패딩 코드가 컴파일에서 빠졌다) `mPaddingHorizontal`에 쓰는 곳이 dex 어디에도 없다. 목록 스타일 `PreferenceFragmentList.Material`은 좌우를 0으로 둔다. 그래서 이 호출이 바꾸는 것은 레이아웃(오버레이가 24 dp를 준다)의 아래 패딩뿐이다.
- **Settings 홈(17)**: `setPaddingHorizontal`이 `isRunningInFreeformWindow`(캡션 막대 인셋이 보이는지)일 때만 `dashboard_padding_bottom`(16 dp)을 아래에 준다. DSA 창의 캡션은 호스트가 게스트 밖에 그리므로 게스트에는 캡션 막대 인셋이 없고, 아래 패딩은 0이다. 이 dimen을 읽는 것은 17 Settings 전체에서 이 메서드 하나다(dex와 컴파일된 XML 모두 확인).
- **모든 앱 목록**: `manage_applications_apps.xml`의 RecyclerView에는 패딩이 없고 코드도 패딩을 정하지 않는다. 16과 17 모두 같다. 같은 레이아웃의 다른 요소가 `paddingBottom`을 써서 이름이 이미 파일에 있다.
- **PermissionController**: 목록은 androidx의 `preference_recyclerview`(`paddingBottom` 0dp)다. 앱이 `<overlayable>`을 선언하므로 idmap2는 목록에 없는 리소스의 오버레이를 거부한다(`ResourceMapping.cpp`의 `CheckOverlayable`). 앱은 permission APEX 안에 있다. `sign_target_files_apks`는 이 APEX를 서명할 때 이미 페이로드를 `deapexer`로 풀고, 안의 APK를 서명하고, `apexer --payload_only`로 다시 싸고, 압축 APEX면 `apex_compression_tool`로 다시 압축한다(`apex_utils.ApexApkSigner`). 같은 도구가 otatools에 있다.
- **파일(16)**: `DirectoryFragment.updateLayout`이 목록 아래를 `getSaveLayoutHeight()`(Material 3면 `bottom_container`, 아니면 `container_save`의 높이)로 채우고, 그 막대가 없으면 0이다. 17은 목록 모드에서 `list_container_padding_bottom`(48 dp)을 쓰고 이 메서드가 없다. 16 DocumentsUI에는 24 dp 리소스로 쓸 만한 필드 참조가 없지만, `getResources`, `getDisplayMetrics`, `DisplayMetrics.density`의 참조는 있다.
- **dexopt**: 16 target_files에서 앱의 odex가 자기 파티션에 있는 것은 KeyChain, WallpaperBackup, FusedLocation, InputDevices, SettingsProvider, Telecom(모두 `speed`, 사유 `prebuilt`)과 CuttlefishService(`verify`, DSA가 지운다)다. Settings와 DocumentsUI를 포함한 나머지는 `SYSTEM_OTHER`에만 있고(`verify`) DSA 디스크에는 실리지 않는다. 서명은 APK의 dex를 바꾸지 않으므로(엔트리의 CRC가 같다) 앞의 앱들의 vdex는 서명 뒤에도 맞는다. Settings와 DocumentsUI는 2026-11-07의 릴리스 빌드에서 `speed`로 컴파일되었다(로그의 `compiled SYSTEM_EXT/priv-app/Settings/Settings.apk with speed: 52430 KiB`, 4개 이미지 모두). APEX 안의 PermissionController는 공개 빌드도 dexpreopt하지 않는다.

### 결정
- Settings 16 홈은 그 `setPadding` 호출을 건너뛴다. Settings 17 홈은 `isRunningInFreeformWindow`가 참을 돌려주고, Settings 오버레이가 `dashboard_padding_bottom`을 24 dp로 바꾼다(그 dimen을 읽는 Settings에만 넣는다. 16에 넣으면 idmap2가 부팅마다 경고한다).
- 모든 앱 목록과 PermissionController는 APK 안의 컴파일된 레이아웃에 `paddingBottom` 24 dp를 넣거나 바꾼다. dex를 고칠 일이 아니다: 패딩이 레이아웃의 것이고 코드가 덮어쓰지 않는다. permission APEX는 서명 전에 서명과 같은 방법으로 풀고 다시 싸며, 서명 뒤에 다시 풀어 편집이 남았는지 확인한다.
- 파일 16은 `getSaveLayoutHeight`를 같은 크기의 코드로 바꿔 막대 높이와 24 dp(밀도로 환산, `getDimensionPixelSize`와 같은 반올림) 중 큰 값을 돌려준다.
- 공개 빌드의 odex 목록을 이미지의 target_files에 `META/dsa_prebuilt_oat.json`으로 남기고, dex가 바뀐 앱은 같은 필터, 같은 공유 라이브러리, 같은 부트 이미지로 다시 컴파일한다. `check-image.py`가 이 목록으로 이미지를 확인한다.

### 확인하지 못한 것
- 소유자 PC에서 Settings의 `oat/`가 없었다는 관찰은 위 릴리스 로그와 맞지 않는다. 설치된 이미지의 버전과, 실제 위치(`/system_ext/priv-app/Settings/oat/x86_64/`)를 PC에서 확인해야 한다.
- 고친 메서드를 ART가 받아들이는지는 이 PC에서 돌려 보지 않았다. CI의 이미지 빌드에서 dex2oat가 Settings와 DocumentsUI를 `--abort-on-hard-verifier-error`로 컴파일하므로, 검증기가 거부하는 메서드가 있으면 빌드가 멈춘다. 화면의 여백은 실제 PC에서 확인한다.

## R21. USB로 연결한 안드로이드 폰에서 게스트를 돌리고, 그리기와 창은 PC가 맡을 수 있는가 (두 번째 VM 백엔드)

**기준.** 2026-11-08. 기기에서 실행해 보지 않았다. 아래 항목은 **[소스]**(코드나 정책 파일을 직접 읽어 확인), **[문서]**(공식 문서), **[보도]**(언론과 커뮤니티 보고), **[추론]**(읽은 것에서 이끌어 낸 것) 중 하나로 표시한다.

### 질문

Windows의 Hyper-V VM 대신 USB로 연결한 폰의 AVF(Android Virtualization Framework) VM에서 DSA 게스트 이미지(arm64)를 돌린다. 폰은 CPU, 메모리, ART, 앱만 맡는다. gfxstream 스트림과 제어 채널은 USB로 PC에 가고, PC의 GPU가 그리며, PC가 지금처럼 태스크마다 창 하나를 띄운다. 폰 자신의 앱은 관여하지 않고, 폰은 잠긴 채 화면이 꺼져 있다.

### 확인한 자료

- AOSP `platform/packages/modules/Virtualization`의 `android17-release`(22f1c9e, 2026-04-24)와 공개 `main` 미러(175a51b, 2025-03-26). 읽은 곳: `android/android.system.virtualmachine.res/AndroidManifest.xml`, `android/virtmgr/src/virtualmachine.rs`와 `crosvm.rs`, `android/virtualizationservice/src/aidl.rs`와 AIDL(`VirtualMachineRawConfig`, `GpuConfig`, `IVirtualMachine`), `android/vm/src/`, `libs/vmconfig`, `libs/libavf`, `android/TerminalApp`, `build/debian/vm_config.json`, `build/microdroid/`, `guest/pvmfw/`, `docs/custom_vm.md`와 `getting_started.md`, `tests/`의 `@VsrTest`.
- AOSP `platform/system/sepolicy` `android17-release`(e066568, 2026-04-20)와 `android16-release`: `private/shell.te`, `crosvm.te`, `virtualizationmanager.te`, `vmlauncher_app.te`, `untrusted_app_all.te`, `te_macros`, `seapp_contexts`.
- `frameworks/base` `android16-release`(99b01a6): `PermissionManagerServiceImpl.java`, `PowerManagerService.java`, `packages/Shell/AndroidManifest.xml`.
- `packages/modules/adb` `android16-release`와 `android17-release`(9084198): `transport.cpp`, `sockets.cpp`, `adb.h`.
- 커널 `kernel/common` `android14-6.1`의 `f_accessory.c`, `android15-6.6`의 `android_f_accessory.c`.
- DSA가 쓰는 gfxstream(d047a572)의 `host/RenderControl.cpp`, `host/virtio-gpu-gfxstream-renderer.cpp`, `guest/renderControl_enc/`. DSA의 `dsa/graphics/patches/0001`, `0003`, `0008`과 `dsa/guest/*/src/host_link.cpp`.
- 보도와 글: Android Authority의 VSR 의무화, Snapdragon, Galaxy S26, One UI 8.5 기사, lyc8503의 Snapdragon Gunyah 글, besworks의 Pixel 사용자 빌드 `vm run` 글, Qualcomm 지원 포럼, XDA의 adb 속도 글, scrcpy PR #646. 출처는 절 끝에 모았다.

### 1. 어느 폰이 사용자 정의 커널과 디스크를 쓰는 비보호(non-protected) VM을 돌리는가

- **VSR은 AVF 자체만 요구하고, 비보호 VM은 요구하지 않는다.** [소스] `MicrodroidCapabilitiesTest.avfIsRequired`(`VSR-7.1-001.004`)는 vendor API 레벨이 202404(Android 15로 출시한 칩셋) 이상이면 `FEATURE_VIRTUALIZATION_FRAMEWORK`를 요구한다. 시계, 자동차, TV는 빠진다. 같은 파일의 다른 시험은 "보호 VM과 비보호 VM 중 적어도 하나"만 요구한다. 그래서 보호 VM만 되는 칩도 VSR을 만족한다. [보도] Android Authority가 같은 내용을 보도했고(대상 칩: Snapdragon 8 Elite, Dimensity 9400, Exynos 2500), Google 직원이 "Android 16부터 AVF가 필수"라고 말했다는 보도도 있다.
- **지원 여부는 `ro.boot.hypervisor.vm.supported`(비보호)와 `ro.boot.hypervisor.protected_vm.supported`(보호)가 정한다.** [소스] `vm info`가 `hypervisor_props`로 이 둘을 읽는다. 비보호가 거짓이면 Terminal 앱은 "Non-protected VMs are not supported on this device"로 실패한다.
- **Google Pixel.** [문서] AVF 문서는 Pixel 6, 6 Pro, 7, 7 Pro, Fold, Tablet을 대상으로 적는다. Pixel 6과 6 Pro에는 AVF가 들어 있지만 기본으로 꺼져 있고, 켜려면 Android 14 이상에서 `fastboot flashing unlock`과 `fastboot oem pkvm enable`이 필요하다(부트로더 잠금 해제, 데이터 초기화). [보도] Terminal 앱은 Android 15 QPR2부터 Pixel에 들어갔고, Pixel 10은 Android 16 QPR2에서 Terminal의 GPU 가속(gfxstream)까지 켠다. Pixel 7 이후(Tensor G2 이후)는 잠금 해제 없이 비보호 VM이 된다고 본다([보도]와 [추론], 기기로 확인하지 않음).
- **Samsung.** [보도] 비보호 VM이 되는 것은 Exynos와 MediaTek 기기뿐이다. Galaxy Z Flip7(Exynos 2500)이 Pixel 아닌 첫 Terminal 지원 폰이었고, Galaxy S26과 S26+(Exynos 2600), A56(Exynos 1580), Tab S11(Dimensity 9400+)이 된다. Snapdragon인 S25 시리즈, S26 Ultra, Z Fold7은 안 된다. One UI 8.5는 Terminal에 그래픽 앱과 저장소 접근을 더했다. Knox 때문이라는 추측은 확인되지 않았다.
- **Qualcomm.** [보도] Snapdragon은 pKVM이 아니라 Qualcomm의 Gunyah 하이퍼바이저를 쓰고, 출시된 모든 Snapdragon(8 Elite Gen 5까지)이 보호 VM만 지원한다. 비보호 VM을 열어 달라는 요청이 Qualcomm 포럼에 있지만 답이 없다. [소스] crosvm에는 Gunyah 백엔드가 있다. 보호 VM으로 우회하는 길은 아래 3에 적는다.
- **MediaTek.** [보도] Dimensity 9400, 9400+ 이후가 비보호 VM을 지원한다. Xiaomi 15T Pro(MediaTek)는 Terminal이 돌고, 같은 회사의 Snapdragon인 Xiaomi 17 Pro는 실행하면 죽는다. 그 밖의 OEM(OPPO, vivo, OnePlus의 Dimensity 기기)은 확인하지 못했다.

### 2. 누가 VM을 만들 수 있는가, 어떤 제한이 있는가

- **권한.** [소스] `MANAGE_VIRTUAL_MACHINE`은 `signature|preinstalled|development`, `USE_CUSTOM_VIRTUAL_MACHINE`은 `signature|development`, `DEBUG_VIRTUAL_MACHINE`은 `signature`다. `virtmgr`는 raw 설정(Microdroid가 아닌 모든 VM)을 "custom"으로 보고 `USE_CUSTOM_VIRTUAL_MACHINE`을 확인한다(`is_custom_config`).
- **shell(adb)은 두 권한을 가진다.** [소스] `packages/Shell/AndroidManifest.xml`이 `MANAGE_VIRTUAL_MACHINE`, `USE_CUSTOM_VIRTUAL_MACHINE`, `DEBUG_VIRTUAL_MACHINE`, `WAKE_LOCK`, `DEVICE_POWER`, `INTERNET`을 요청한다. `shell.te`의 `virtualizationservice_use(shell)`은 `userdebug_or_eng`로 감싸여 있지 않으므로 사용자 빌드에도 있다.
- **사용자 빌드에서 root 없이 `vm run`이 된다(소스로는 그렇고, 기기 보고 하나가 맞는다).** [소스] `vm` 도구는 JSON의 커널, initrd, 디스크를 스스로 열어 fd로 넘긴다(`vmconfig::to_parcelable`의 `open_parcel_file`). crosvm은 `shell_data_file`을 `open`할 수는 없지만(사용자 빌드), 넘겨받은 fd로 읽고 쓰는 것은 허용된다(`crosvm.te`의 `{ getattr read ioctl lock }`와 `shell_data_file:file write`). 그래서 `/data/local/tmp`의 파일로 VM을 띄울 수 있다. [문서] `custom_vm.md`는 "rooted AVF-enabled device"와 `adb root`를 예로 들지만, 이는 예시 절차이고 코드가 root를 요구하지는 않는다. [보도] besworks는 Android 15 Pixel 사용자 빌드에서 root 없이 `adb shell /apex/com.android.virt/bin/vm run`으로 Arch VM을 부팅했고, 네트워크(TAP)만 되지 않았다고 적었다.
- **일반 앱도 개발 권한을 받으면 만들 수 있다.** [소스] 두 권한은 `development` 보호 수준이고, `PermissionManagerServiceImpl`은 개발 권한을 `pm grant`로 주는 것을 허용한다(testOnly 확인은 그 경로에 없다). `untrusted_app_all.te`도 `virtualizationservice_use`를 가진다. 다만 Java API(`android.system.virtualmachine`)는 `@SystemApi`이고, 사용자 정의 이미지 설정(`VirtualMachineCustomImageConfig`)은 숨은 API다. 일반 앱이 쓰려면 숨은 API 제한을 넘거나, 클라이언트가 `virtmgr`를 직접 실행하는 방식(`te_macros`의 `domain_auto_trans($1, virtualizationmanager_exec, ...)`)으로 안정되지 않은 AIDL을 직접 말해야 한다. 안정된 C API인 `libavf`(`AVirtualMachine_createRaw`, `AVirtualMachine_connectVsock`)는 LLNDK라 벤더 프로세스용이다.
- **Linux Terminal 앱.** [소스] `com.android.virtualization.terminal`은 platform 서명 priv-app이고 SELinux 도메인이 `vmlauncher_app`이다. 이 도메인만 `vsock_socket`을 직접 만들고 `listen`, `accept`할 수 있다(`RELEASE_AVF_ENABLE_NETWORK` 플래그일 때). Terminal은 `specialUse` 포그라운드 서비스로 VM을 유지한다.
- **vsock 방향 제한이 설계를 바꾼다.** [소스] `te_macros`는 클라이언트에게 `virtmgr`가 만든 vsock fd의 `read`, `write`만 주고 "we do not grant permission to create a vsock"라고 적는다. 즉 shell이나 일반 앱은 호스트 쪽(CID 2)에서 vsock을 `listen`할 수 없고, `IVirtualMachine.connectVsock(port)`로 **폰 쪽에서 게스트로** 연결만 할 수 있다. 지금 DSA 게스트의 모든 연결(gfxstream의 `HvSocketStream`, hwcomposer, 오디오, 카메라, 센서, codec2, channeld)은 게스트가 `VMADDR_CID_HOST`로 거는 방향이므로 그대로는 받을 쪽이 없다.
- **메모리.** [소스] `memoryMib`의 기본은 256 MiB이고 상한 확인은 `virtmgr`에 없다. 실제 상한은 폰의 여유 메모리다. Terminal은 4096 MiB를 쓴다. 메모리 반환은 `balloon`을 요청하고 기기가 `hypervisor.memory_reclaim.supported`를 참으로 둘 때만 `--balloon-page-reporting`으로 켜진다. DSA가 Hyper-V에서 쓰는 빈 페이지 보고와 같은 방식이지만, 기기마다 켜졌는지 확인해야 한다.
- **CPU.** [소스] `CpuCount(n)`이나 `MatchHost`(`--host-cpu-topology`, arm64는 `--virt-cpufreq`)를 고른다. `vm` 도구의 기본은 vCPU 1개다.
- **장치.** [소스] 디스크(raw 이미지나 파티션으로 만든 composite 디스크, virtio-blk), 공유 경로(virtio-fs, vhost-user), 콘솔, 입력, 오디오(vhost-user snd), USB 패스스루(`UsbConfig`), 네트워크(TAP, `createTapInterface`가 `INTERNET`과 `USE_CUSTOM_VIRTUAL_MACHINE`을 확인), GPU와 디스플레이(`GpuConfig`, `DisplayConfig`)가 있다. GPU와 디스플레이는 `paravirtualized_devices` 빌드 플래그가 없으면 경고만 내고 무시된다. Terminal의 gfxstream 백엔드(`gfxstream-vulkan`, `gfxstream-composer`)는 **폰의 GPU**로 그리는 것이라 이 질문의 목적에는 쓰지 않는다. DSA는 이미 virtio-gpu 없이 스트림만으로 gfxstream을 돌린다(패치 0008, 0003).

### 3. DSA 안드로이드 게스트가 AVF VM에서 부팅할 수 있는가

- **안드로이드 계열 게스트가 AVF에서 도는 것은 이미 있다.** [소스] Microdroid는 "a (very) lightweight version of Android"(같은 소스, init, apexd, binder 사용)이고 AVF의 기본 게스트다. [보도] Google은 Pixel에서 ChromeOS(Ferrochrome)를 AVF로 시연했고, crosvm은 ChromeOS의 ARCVM(전체 안드로이드)과 Cuttlefish의 VMM이다. 전체 AOSP 시스템 이미지를 폰의 AVF로 부팅한 공개 사례는 찾지 못했다.
- **DSA 이미지를 그대로 쓸 수는 없다.** [추론] 지금의 ARM64 이미지(R10)는 Hyper-V용이다. 커널은 ACPI, VMBus, `hv_sock`, Hyper-V SCSI, PL011을 쓴다. AVF의 crosvm은 장치 트리, virtio-pci, virtio-blk, virtio-vsock, 8250 UART(`console=ttyS0`), GICv3, arch timer를 준다. binder 내장, memfd, cgroup 패치는 아키텍처나 하이퍼바이저와 무관하므로 그대로 쓴다(R10의 표). 디스크는 SCSI 순서가 아니라 composite 디스크의 파티션 GUID와 라벨로 찾을 수 있다(`vm_config.json`의 `partitions[].guid`, R11과 같은 방식). `androidboot.boot_devices`도 PCI 경로로 바뀐다(Microdroid의 `bootconfig.arm64`가 `2e000000.pci`를 쓴다).
- **보호 VM으로는 사용자 커널을 띄울 수 없다(Snapdragon의 유일한 길).** [소스] pvmfw는 게스트 커널을 AVB 해시 푸터로 검증하고, 신뢰하는 키는 빌드에 박힌 `pvmfw_embedded_key_pub.bin`과 설정의 추가 키뿐이다. 설정의 `customPvmfw`는 raw 설정에서 받지만 이것이 pKVM이나 Gunyah에서 실제로 보호 VM의 펌웨어를 바꾸는지는 확인하지 못했다. [보도] lyc8503은 OnePlus 13(8 Elite)에서 root로 crosvm을 직접 `--protected-vm-without-firmware`로 실행해 Debian과 Windows를 띄웠다. 1 GiB가 넘는 메모리는 자주 "Out of memory"로 실패해 hugepage 예약 모듈이 필요했고, virtio는 `CONFIG_DMA_RESTRICTED_POOL`(모든 I/O가 bounce 버퍼를 지난다)을 켜야 했다. 보호 VM의 메모리는 호스트에 빌려 준 것이라 유휴 메모리를 호스트가 돌려받는다는 DSA의 제약(INTENT §2)과도 맞지 않는다. 그래서 Snapdragon은 root 없이도, 제약 안에서도 대상이 아니다.

### 4. 화면을 끄고 잠근 채 계속 도는가

- **기기가 잠들면 VM도 멈춘다.** [소스] Android 17의 Terminal(`VmService.kt`)은 앱이 백그라운드로 가면 사용자가 고른 시간 동안 `PARTIAL_WAKE_LOCK`("TerminalApp:KeepAwake")을 잡는다. Google이 이 설정을 따로 둔 것은 웨이크 락 없이 화면이 꺼지면 커널 suspend로 vCPU가 멈춘다는 뜻이다([추론]).
- **shell UID의 웨이크 락은 Doze가 끄지 않는다.** [소스] `PowerManagerService.setWakeLockDisabledStateLocked`는 앱 UID(`>= FIRST_APPLICATION_UID`)의 부분 웨이크 락만 Doze, 캐시 상태, 저전력 대기에서 끈다. shell(2000)은 해당되지 않는다. 앱이라도 포그라운드 서비스 상태(`PROCESS_STATE_BOUND_FOREGROUND_SERVICE` 이하)면 Doze 중에도 꺼지지 않는다. shell은 `WAKE_LOCK`과 `DEVICE_POWER`를 가진다. 플랫폼 정책은 `/sys/power/wake_lock` 쓰기를 `system_suspend`와 `init`에만 주므로 `PowerManager`를 거쳐야 한다.
- **충전 중에는 깊은 Doze에 들어가지 않는다.** [문서] Doze는 기기가 전원에 연결되지 않았을 때만 시작한다. USB로 PC에 꽂혀 있으면 충전 상태이므로 Doze의 네트워크, 알람 제한은 사실상 걸리지 않는다. 다만 PC 포트가 충전 전류를 충분히 주지 못하면 배터리가 줄 수 있다([추론]).
- **앱 대기 버킷, 캐시 프리저, 팬텀 프로세스 정리.** [추론] 셋 다 앱 프로세스 대상이다. adb에서 `setsid`로 띄운 shell 프로세스와 그 자식(`virtmgr`, crosvm)은 해당되지 않는다. 일반 앱으로 띄우면 `virtmgr`와 crosvm이 그 앱의 자식이므로, 포그라운드 서비스가 아니면 팬텀 프로세스 정리와 캐시 프리저의 대상이 될 수 있다. 기기에서 확인하지 않았다.
- **VM의 수명은 클라이언트 프로세스에 묶인다.** [소스] 클라이언트가 `virtmgr`를 실행하고, 클라이언트는 그 프로세스 그룹을 죽일 수 있다(`te_macros`의 `sigkill`). [추론] 그러므로 VM을 띄운 프로세스가 USB 분리, adb 재시작에도 살아 있어야 한다. `adb shell`의 세션 종료는 SIGHUP을 보내므로 `setsid`로 떼어 낸 상주 프로세스가 필요하다.
- **메모리 압박.** [추론] 비보호 VM의 메모리는 crosvm의 익명 메모리이므로 폰 커널이 zram으로 내보낼 수 있고, lmkd가 고르는 대상은 프로세스의 `oom_score_adj`에 달렸다. shell에서 띄운 프로세스의 값과 lmkd가 이를 죽이는지는 확인하지 못했다.
- **발열.** [보도] lyc8503의 시험에서 8 Elite는 VM 안의 커널 컴파일로 SoC가 105°C에 이르러 중단했다. 충전 중 지속 부하는 폰의 열 관리가 CPU 클럭을 낮추게 하므로, 지속 성능은 처음 몇 분의 성능보다 낮다고 보아야 한다. 화면이 꺼져 있는 것은 열 예산에 유리하다([추론]).
- **잠금과 저장소.** [추론] 이미지를 `/data/local/tmp`(기기 암호화 영역)에 두면 재부팅 뒤 처음 잠금을 풀기 전에도 읽을 수 있다. 앱의 자격 증명 암호화 영역에 두면 첫 잠금 해제 전에는 VM을 띄울 수 없다. USB 디버깅이 켜져 있고 PC 키가 승인되어 있어야 한다. 기기에 따라 adb 승인이 7일 뒤 만료되는 설정이 있다.

### 5. 전송 경로와 비용

경로는 게스트(vsock) → 폰의 중계 프로세스 → USB → PC의 DSA 호스트다. USB 구간의 후보는 셋이다.

- **adb forward.** [소스] adb 패킷의 최대 페이로드는 1 MiB(`MAX_PAYLOAD`)다. 흐름 제어가 중요하다. 수신 쪽 확인을 미루는 `delayed_ack` 기능이 없으면, 로컬 소켓은 `WRTE` 하나를 보낸 뒤 `OKAY`가 올 때까지 읽기를 멈춘다(`sockets.cpp`의 "acks not deferred, blocking"). 그동안 쌓인 바이트는 다음 패킷에 합쳐지지만, 소켓마다 한 번에 패킷 하나만 날아가므로 어떤 메시지든 앞 패킷의 왕복을 기다릴 수 있다. adbd는 `delayed_ack`를 늘 알리지만, PC 쪽 adb는 환경 변수 `ADB_BURST_MODE=1`일 때만 켠다(`android17-release`에서도 같다). 켜면 소켓마다 32 MiB(`INITIAL_DELAYED_ACK_BYTES`)까지 확인 없이 보낸다. DSA가 adb를 쓴다면 자기 adb 서버를 이 변수로 띄우거나, ADB 프로토콜을 직접 말해야 한다. [보도] 처리량: USB 3 케이블에서 adb push와 pull이 146~202 MB/s(Xiaomi 13 Ultra, Xperia 5 II), USB 2에서 약 39.5 MB/s다. USB 2 bulk의 실효 상한은 약 38 MB/s다.
- **AOA(USB 액세서리 모드).** [소스] 커널의 액세서리 기능은 16 KiB 버퍼, 송신 요청 4개, 수신 요청 2개를 쓰고 SuperSpeed 서술자를 가진다(`android14-6.1`의 `f_accessory.c`, `android15-6.6`의 `android_f_accessory.c`). 앱이 `UsbManager.openAccessory`로 열 수 있어 adb와 개발자 옵션 없이 쓸 수 있다. 측정된 처리량은 찾지 못했다. 다만 VM 생성 자체에 `pm grant`(adb)나 shell이 필요하므로 adb를 완전히 없애지는 못한다.
- **USB 테더링(NCM).** [문서와 보도] Pixel은 NCM을 쓰고 USB 3에서 링크 속도가 수 Gbps다. 측정된 처리량과 지연은 찾지 못했다. 사용자가 테더링을 켜야 하고 PC의 네트워크 구성을 바꾸므로 기본 경로로는 맞지 않는다고 본다([추론]).

**왕복 시간 추정 [추론, 측정하지 않음].** USB 2 고속 bulk는 125 µs 마이크로프레임으로 일정을 잡고, USB 3은 그보다 짧다. 하지만 adb 경로는 게스트 vsock → 중계 프로세스 → adbd(FunctionFS) → USB → PC의 adb 서버 → TCP 루프백 → DSA로 사용자 공간을 여러 번 지나고, 화면이 꺼진 폰의 CPU는 깊은 유휴 상태에서 깨어나야 한다. 작은 메시지의 왕복을 USB 3에서 0.3~1 ms, USB 2에서 0.5~2 ms로 추정한다. 같은 PC 안의 hvsocket 왕복은 재지 않았지만 수십 µs 수준으로 본다. DSA가 잰 호스트 왕복 `wm.frame`(게스트에서 호스트로 가서 호스트 일을 하고 돌아오는 것)은 중앙 0.8 ms였다(위 "터치 지연의 구간별 실측"). USB 경로는 동기 호출마다 대략 0.3~2 ms를 더한다.

**gfxstream이 왕복을 줄이는 수단 [소스].** 호스트 기본값으로 `vkQueueSubmitAsyncGOOGLE`(비동기 제출), `VulkanQueueSubmitWithCommands`(명령 버퍼 기록을 제출 때 몰아 보냄), `VulkanBatchedDescriptorSetUpdate`(디스크립터 갱신을 제출 때까지 미룸)가 켜진다. `vkBeginCommandBufferAsyncGOOGLE`, `vkEndCommandBufferAsyncGOOGLE`, `vkResetCommandBufferAsyncGOOGLE`, `vkQueueWaitIdleAsyncGOOGLE`, `vkQueueSignalReleaseImageANDROIDAsyncGOOGLE`가 비동기다. `VulkanAsyncQsri`는 `GlAsyncSwap`이나 (`VirtioGpuNativeSync`와 `VirtioGpuFenceContexts`)가 켜질 때만 켜진다. DSA는 `VirtioGpuNext`만 끈다. 남는 동기 호출은 핸들이나 결과를 돌려받는 것들이다: `vkCreate*`, `vkAllocate*`, 펜스 대기와 상태, 메모리 요구 사항 질의, renderControl의 색 버퍼 생성.

**프레임 시간 영향 추정 [추론].**

| 경우 | 계산 | USB 3 | USB 2 |
| --- | --- | --- | --- |
| 정상 상태 UI 프레임 | 동기 호출 1~3개 × 왕복 + 제출당 8~25 KB(DSA 실측) | 약 0.5~3 ms 추가 | 약 1~7 ms 추가 |
| 앱 시작, 창 크기 변경 | 파이프라인, 이미지, 메모리, 색 버퍼 생성이 수백~수천 번 동기 호출 | 수백 ms~1 s 넘게 추가 | 그 두 배 |
| 큰 비트맵 하나(1080×2400 RGBA, 10.4 MB) | 처리량 | 약 50~70 ms | 약 270 ms |
| 게스트 안 동영상 디코드(1080p60 NV12, 약 187 MB/s) | 처리량 | adb 상한 근처 | 불가능 |

- 정상 상태 UI는 16.7 ms 예산 안에 들어가지만, 지금도 hwcomposer compose의 p90이 13 ms이므로 놓치는 프레임이 늘어난다.
- 가장 큰 비용은 앱 시작과 크기 변경 때의 동기 호출 수다. 실제 수는 DSA 추적으로 셀 수 있지만 아직 세지 않았다.
- 동영상은 DSA가 이미 codec2를 호스트로 보내므로(`dsa/guest/codec2/src/host_link.cpp`) 압축된 스트림(수 MB/s)만 지나야 한다. 카메라는 PC 카메라의 프레임을 게스트로 넣는 방향이라, 압축하지 않으면 USB 2로는 1080p30(약 93 MB/s)이 지나지 못한다.
- 패치 0003의 host-visible 메모리 복사는 바뀐 4 KiB 블록만 보내므로 지금 전송량은 작다. 다만 게스트가 GPU 결과를 다시 읽는 경우(쿼리 결과, 읽기용 버퍼, 스크린샷)에는 그 데이터가 USB로 돌아온다.
- 터치 입력은 PC에서 폰으로 한 방향이고 응답을 기다리지 않으므로 지연이 대략 USB 편도만큼 는다.

### 6. 판단

**root 없이 가능하다. 다만 개발자 옵션과 USB 디버깅이 필요하고, 비보호 VM을 지원하는 기기에서만 된다.** 근거: shell이 권한, SELinux, fd 전달 모두에서 사용자 정의 VM을 만들 수 있고([소스]), 같은 일이 사용자 빌드 Pixel에서 보고되었다([보도]).

- **대상 기기**: Pixel 7 이후(Pixel 6, 6 Pro는 부트로더 잠금 해제와 `fastboot oem pkvm enable`이 필요), Exynos 2500, 2600, 1580 Galaxy(Z Flip7, S26, S26+, A56), Dimensity 9400+ 이후 기기(Galaxy Tab S11, Xiaomi 15T Pro). **대상이 아닌 기기**: 모든 Snapdragon 기기(Galaxy의 Snapdragon 모델 포함)와 AVF 이전 칩셋.
- **주요 위험**:
  1. 앱 시작과 크기 변경의 동기 호출 왕복. 측정 전에는 사용성을 장담할 수 없다.
  2. 기기 범위가 좁고 OEM이 정한다. Snapdragon이 비보호 VM을 열지는 Qualcomm에 달렸다.
  3. AVF의 클라이언트 AIDL은 안정 API가 아니고 APEX 업데이트로 바뀐다. 안정 C API(`libavf`)는 벤더 전용이다.
  4. 폰 쪽에서 vsock을 `listen`할 수 없으므로 게스트의 모든 연결 방향을 바꾸는 중개가 필요하다.
  5. 장시간 지속 부하의 발열과 클럭 저하, USB 포트 전류 부족.
  6. 메모리 반환은 기기의 `hypervisor.memory_reclaim.supported`에 달렸다.
  7. 설치 크기: 시스템, 벤더, 데이터 디스크를 폰 저장소에 올려야 한다(수 GB).
  8. CI에 폰이 없으므로 검증은 소유자의 기기와 수동 점검에 의존한다.
- **두 번째 백엔드에 필요한 구성 요소**:
  1. **게스트 커널 구성**: crosvm arm64용(장치 트리, virtio-pci, virtio-blk, virtio-vsock과 `CONFIG_VSOCKETS_LOOPBACK`, 페이지 보고 balloon, 8250 콘솔, PSCI). binder 내장과 기존 패치는 유지하고 Hyper-V 장치는 뺀다.
  2. **게스트 이미지 변형**: virtio-blk와 파티션 GUID로 찾는 fstab과 `androidboot.boot_devices`, 메모리와 CPU 기본값, AVF의 `vm_config` 형식에 맞춘 composite 디스크 구성.
  3. **게스트 쪽 연결 중개**: 게스트 구성 요소가 `VMADDR_CID_HOST` 대신 게스트 안의 중개자(`VMADDR_CID_LOCAL`이나 유닉스 소켓)에 연결한다. 중개자는 폰이 걸어 온 vsock 연결 하나 위에서 다중화하거나, 제어 연결로 새 연결을 요청한다. gfxstream의 `HvSocketStream`과 각 `host_link`는 주소만 바꾸면 된다.
  4. **폰 쪽 실행기와 중계 프로세스**: adb로 올려 `setsid`로 상주하는 shell 프로세스(네이티브 바이너리나 `app_process` 자바 프로그램)다. `virtmgr`를 띄워 raw 설정으로 VM을 만들고, `PowerManager` 부분 웨이크 락을 잡고, `connectVsock`으로 게스트에 연결해 adb의 `localabstract` 소켓과 잇는다. VM 상태와 발열 이벤트를 PC에 알린다. USB가 끊겨도 VM은 계속 두고 다시 연결을 기다린다.
  5. **배포와 갱신**: 이미지를 `/data/local/tmp`에 올리고 해시로 확인하고, 데이터 디스크를 보존하며 갱신하고, 지울 때 남김없이 지운다.
  6. **PC 쪽 백엔드**: 기기 찾기와 승인 안내, `ADB_BURST_MODE=1`로 띄운 자체 adb 서버(Apache-2.0)나 libusb 위의 ADB 구현, 지금 hvsocket 서비스 포트를 전달 스트림으로 바꾸는 전송 계층, gfxstream 렌더러와 제어 채널이 hvsocket 대신 그 스트림을 읽도록 하는 추상화. 플랫폼 중립이어야 하므로(D9) 전송 계층은 `dsa/host/core/`의 경계 뒤에 둔다.
  7. **수명 주기 연결**: 게스트 Doze는 그대로 쓰고, Freeze는 crosvm `suspend`로 한다. 폰 분리, 발열, 저전력을 호스트 상태로 알린다.
  8. **진단**: 동기 호출 수와 USB 왕복을 재는 추적 구간. 이것으로 위 추정을 실측으로 바꾼다.

### 확인하지 못한 것

- 실제 기기에서의 부팅, USB 왕복, 처리량, 앱 시작 시간. 이 조사에서는 기기를 쓰지 않았다.
- `customPvmfw`로 Snapdragon의 보호 VM에 사용자 커널을 root 없이 띄울 수 있는지.
- shell에서 띄운 crosvm의 `oom_score_adj`와 lmkd의 동작, 각 OEM의 백그라운드 정리(특히 Samsung, Xiaomi)가 shell 프로세스를 건드리는지.
- Pixel 7 이후 각 모델과 Dimensity 기기의 `hypervisor.memory_reclaim.supported` 값.
- AOA와 NCM의 실측 처리량과 지연.

### 결정

결정은 소유자의 몫이다(INTENT를 바꾸는 일이다). 이 조사가 권하는 다음 단계는 기기 하나(Pixel 8 이후나 Exynos 2600)에서 두 가지를 재는 것이다. 하나는 DSA arm64 커널로 crosvm VM을 shell에서 띄우는 것이고, 다른 하나는 adb forward(burst 켬과 끔) 위의 작은 메시지 왕복과 처리량이다. 이 두 수치가 있어야 앱 시작 비용을 계산할 수 있다.

2026-11-08 소유자가 이 백엔드를 정식으로 받아들였다(INTENT D26, SPEC FR-33, PROJECT M10). 구현은 M9 뒤에 위의 측정부터 시작한다.

폰의 안드로이드 자체에서 앱을 띄워 PC로 보내는 방식은 하지 않는다. 폰을 잠근 채 쓰려면 잠금을 무시하는 가상 화면이 필요한데, 그 화면은 PC 앞의 누구에게나 그 앱을 잠금 없이 여는 것과 같다. 이 백엔드의 VM은 폰과 별개의 시스템이라 폰의 잠금이 폰의 앱과 데이터를 그대로 지킨다.

### 출처

- AVF 문서: https://source.android.com/docs/core/virtualization , https://android.googlesource.com/platform/packages/modules/Virtualization/+/refs/heads/android17-release/docs/custom_vm.md , https://android.googlesource.com/platform/packages/modules/Virtualization/+/refs/heads/android17-release/docs/getting_started.md
- VSR 의무화: https://www.androidauthority.com/android-15-virtual-machine-mandate-3498428/ , https://x.com/MishaalRahman/status/1887535813811908846
- 칩셋별 Terminal 지원: https://www.androidauthority.com/snapdragon-chips-android-linux-terminal-3608648/ , https://www.androidauthority.com/galaxy-s26-linux-terminal-3645822/ , https://www.androidauthority.com/galaxy-z-flip-7-linux-terminal-3578675/ , https://www.androidauthority.com/samsung-phones-linux-terminal-one-ui-8-5-3652503/ , https://www.sammyfans.com/2026/03/03/not-all-samsung-galaxy-s26-phones-support-androids-linux-terminal/
- Terminal GPU 가속: https://www.androidauthority.com/pixel-10-linux-apps-gpu-acceleration-3608754
- Qualcomm Gunyah: https://blog.lyc8503.net/en/post/android-virtualization-on-snapdragon/ , https://mysupport.qualcomm.com/supportforums/s/question/0D5dK00000G7uG5SAJ/ , https://docs.qualcomm.com/bundle/publicresource/topics/80-70020-3/virtualization.html
- 사용자 빌드의 `vm run`: https://dev.to/besworks/official-linux-terminal-on-android-hkf
- Ferrochrome: https://www.androidpolice.com/google-dashes-our-hopes-of-running-chrome-os-on-android/
- adb 처리량: https://xdaforums.com/t/xiaomi-13-ultra-adb-pull-push-speed-file-transfer-speed-via-usb-cable.4606831/ , https://x.com/fabynou/status/1009201231896563712
- scrcpy 지연(영상 경로, 참고만): https://github.com/Genymobile/scrcpy/pull/646
- Doze 조건: https://developer.android.com/training/monitoring-device-state/doze-standby

## R22. 앱이 그리는 제목 표시줄과 뒤로 가기 키: 시스템과 앱이 주고받는 것 (FR-22, FR-5)

### 확인한 자료
- `frameworks/base`의 고정 커밋 두 개: Android 16(빌드 15581820, `45034f0663f960d9ee5fb0a101a4732b71f6e2f4`)과 Android 17(빌드 16373615, `94b4c163b7dfe5ce3607f7bb8456f9573f7de57d`). `framework-lock.json`이 고정한 커밋이다. `frameworks/native`는 Android 16의 `fcbde2bcff56ca1d7bec9cabf8bdca5e288bf103`.
- 읽은 파일: WM Shell의 `DesktopModeWindowDecoration.java`, `DesktopModeWindowDecorViewModel.java`, `WindowDecoration.java`, `WindowDecorationInsets.kt`, `common/CaptionRegionHelper.kt`, `common/WindowDecorationGestureExclusionTracker.kt`, `extension/TaskInfo.kt`. 코어의 `InsetsSource.java`, `WindowContainerTransaction.java`, `WindowInsetsController.java`, `ISystemGestureExclusionListener.aidl`, `IWindowManager.aidl`, `Activity.java`, `InputMethodManager.java`, `KeyGestureEvent.java`, `InputManager.java`, `data/keyboards/Virtual.kcm`, `Generic.kcm`, `core/res/AndroidManifest.xml`. 서비스의 `WindowManagerService.java`, `PhoneWindowManager.java`, `InputManagerService.java`, `KeyGestureController.java`, `InputGestureManager.java`. 네이티브의 `inputflinger/dispatcher/InputDispatcher.cpp`.

### 방법
- 각 파일을 고정 커밋에서 `android.googlesource.com`의 `?format=TEXT`로 받아 `.scratch/caption-research/a16`, `a17`에 두고 읽었다. 같은 파일을 두 버전에서 비교했다.

### 찾은 것
- **투명한 캡션 요청을 아는 법**: 셸의 `TaskInfo.isTransparentCaptionBarAppearance`는 `taskDescription.topOpaqueSystemBarsAppearance`에 `APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND`(`1 << 7`)가 있는지 본다. 앱은 `WindowInsetsController.setSystemBarsAppearance`로 이 비트를 켠다. 16과 17이 같다. 에이전트가 이미 상태 표시줄 밝기를 읽는 `getTopOpaqueSystemBarsAppearance`와 같은 값이다.
- **캡션 요소의 자리를 앱에 알리는 법**: 셸은 캡션을 `WindowContainerTransaction.addInsetsSource(token, owner, index, captionBar(), frame, Rect[] boundingRects, flags)`로 태스크의 인셋으로 준다. `boundingRects`는 캡션 요소(왼쪽의 앱 칩, 오른쪽의 단추들)가 차지하는 곳이고(`CaptionRegionHelper.calculateBoundingRectsInsets`, 요소마다 START나 END에 붙은 너비), 앱은 `WindowInsets.getBoundingRects(captionBar())`로 받는다. 16의 `InsetsSource.calculateBoundingRects`는 `captionBar` 형식의 사각형을 창 기준으로 그대로 쓰고, 17은 모든 형식을 프레임 기준으로 바꾼다. 그래서 사각형을 줄 때는 프레임을 태스크의 가로 범위와 같게 두어야 두 버전에서 뜻이 같다. 17은 `InsetsBoundingRect[]`를 받는 형식을 더했지만 `Rect[]` 형식도 남아 있다.
- **앱의 조작 요소와 끌기 자리를 가르는 법**: 셸은 투명 캡션이면 캡션 창을 `INPUT_FEATURE_SPY`로 두고, `ENABLE_ACCESSIBLE_CUSTOM_HEADERS`이면 캡션의 터치 영역을 시스템 자리로만 줄인다(`calculateLimitedTouchableRegion`). 시스템 자리는 캡션에서, 요소의 사각형을 뺀 "꾸밀 수 있는 영역"과 디스플레이의 시스템 제스처 제외 영역이 겹치는 곳("꾸민 영역")을 뺀 나머지다. 제외 영역은 `IWindowManager.registerSystemGestureExclusionListener`로 받는다(`WindowDecorationGestureExclusionTracker`). 곧 앱은 `View.setSystemGestureExclusionRects`로 조작 요소를 알리고, 알리지 않은 곳은 끌기 자리다. 이 등록에는 권한 검사가 없다(`WindowManagerService` 5222행, 17은 5378행). 리스너는 제한을 둔 영역과 제한 전 영역 두 개를 받는다. 제한은 뒤로 제스처를 위한 화면 가장자리 제한이다.
- **뒤로 단추와 키**: `Virtual.kcm`(16과 17이 같다)에서 `ESCAPE`의 기본 동작은 `none`이고, `DEL`은 `ctrl+alt: fallback BACK`뿐이다. 키 하나가 앱에서 처리되지 않으면 `InputDispatcher::afterKeyEventLockedInterruptable`이 정책의 `dispatchUnhandledKey`를 부르고, `InputManagerService.dispatchUnhandledKey`는 먼저 `KeyGestureController.interceptUnhandledKey`를 부른 뒤 키 맵의 대체 키를 만든다. 처리되지 않은 Escape(조합키 없음, 첫 누름)는 그 단계에서 `KEY_GESTURE_TYPE_CLOSE_ALL_DIALOGS`(61)가 되고, 이 동작을 만드는 다른 길은 없다. `PhoneWindowManager`가 이 동작의 처리기로 늘 등록하므로 동작은 처리되고, `InputManager.registerKeyGestureEventListener`(권한 `MANAGE_KEY_GESTURES`, 서명 보호)의 리스너에 전해진다. 주입한 이벤트의 처리 결과는 주입한 쪽에 돌아오지 않는다(`setInjectionResult`는 처리 여부를 담지 않는다).
- **글자 입력란**: `InputMethodManager.createInputConnection`은 입력란이 아닌 뷰에도 입력을 시작하되 연결이 없으면 `fieldId`를 0으로 지운다. 터미널처럼 `TYPE_NULL`로 키를 직접 받는 입력란은 연결이 있다.
- **첫 화면**: `TaskInfo.numActivities`가 태스크의 액티비티 수다. 1이면 맨 아래 액티비티이고, 그때 뒤로는 태스크를 끝내거나 뒤로 보낸다.

### 결정
- 에이전트가 셸의 일을 한다. DSA 게스트에는 SystemUI와 WM Shell의 데스크톱 장식이 없기 때문이다. 투명 캡션 비트를 태스크마다 읽어 보내고, 그런 앱의 `captionBar` 인셋에 호스트의 아이콘 칸과 캡션 단추의 사각형을 `Rect[]`로 주며, 그때 프레임은 태스크의 가로 범위로 둔다. 제외 영역 리스너를 등록해, 띠 안에서 아이콘 칸과 단추 사이에 든 제외 영역을 태스크 기준 사각형으로 보낸다. 호스트는 그 사각형 위를 `HTCLIENT`로, 나머지 띠를 `HTCAPTION`으로 답한다. 게스트에는 뒤로 제스처가 없으므로 제한 전 영역을 쓴다.
- ESC는 Escape로 앱에 보내고, `CLOSE_ALL_DIALOGS` 동작이 오면(1초 안, 가상 키보드, 키가 Escape) 첫 화면이 아닐 때만 뒤로를 보낸다. Backspace는 입력란에 초점이 없고 첫 화면이 아닐 때 Ctrl과 Alt를 더해 보내, 키 맵의 대체 키가 앱이 쓰지 않은 것만 뒤로로 바꾸게 한다. 둘 다 프레임워크가 "앱이 이 키를 쓰지 않았다"고 아는 유일한 자리를 쓴다. 앱이 키를 썼는지를 에이전트가 추측하는 방법(화면 변화 등)은 믿을 수 없다. 키 맵 파일을 이미지에서 바꾸는 방법은 adb와 다른 주입에도 영향을 주고 이미지 빌드를 바꿔야 해서 쓰지 않았다.
- 마우스의 뒤로와 앞으로 단추는 포인터 누름이 아니라 `KEYCODE_BACK`과 `KEYCODE_FORWARD` 키다. 실제 마우스에서 입력 리더가 하는 일과 같고, 단추 아래의 뷰를 누르지 않는다.

### 확인하지 못한 것
- 이 PC에서 아무것도 실행하지 않았다. 특히 `CLOSE_ALL_DIALOGS` 동작이 주입한 Escape에도 오는지, `MANAGE_KEY_GESTURES`가 플랫폼 서명 앱에 실제로 주어지는지, 제외 영역 리스너가 앱 창의 제외 영역을 이 디스플레이에서 보고하는지는 설치한 이미지에서 확인해야 한다.
- Ctrl과 Alt가 붙은 Backspace를 조합키가 없을 때만 처리하는 앱(`hasNoModifiers`를 보는 게임 등)은 그 Backspace를 쓰지 않아 뒤로 가기가 된다. 실제 앱에서 이런 경우가 있는지 확인해야 한다.
- Chrome이 제외 영역으로 탭을 알리는지는 Chromium 소스나 실제 실행으로 확인해야 한다(FR-22의 수동 확인).

## R23. 시스템 화면 디자인을 정적 오버레이로 바꿀 수 있는 곳과 없는 곳 (FR-34, INTENT D27)

### 확인한 자료
- 빌드 15581820(Android 16)과 16373615(Android 17)의 `manifest_<build_id>.xml`이 고정한 커밋. `frameworks/base`는 `45034f06`과 `94b4c163`(`framework-lock.json`과 같다), `packages/apps/Settings`는 `f6ec09af`와 `213829fb`, `packages/apps/DocumentsUI`는 `ca994547`과 `4568ed0b`, `packages/modules/Permission`은 `e86c3ce1`과 `1093eca7`, `packages/modules/IntentResolver`는 `a2ebe651`과 `514e2233`, `external/roboto-fonts`는 `064c5ab9`와 `48287c08`.
- 읽은 파일: SettingsLib의 `CollapsingToolbarBaseActivity`(레이아웃, `values-v31/dimens.xml`, `values-v36/styles_expressive.xml`, `CollapsingToolbarDelegate.java`)와 `SettingsTheme`(`drawable-v36/settingslib_round_background*.xml`, `values-v36/dimens.xml`, `values-v34/colors.xml`, `themes_expressive.xml`, `SettingsPreferenceGroupAdapter.kt`, `SettingsBasePreferenceFragment.kt`), Settings의 `settings_homepage_container.xml`, `search_bar.xml`, `SettingsHomepageActivity.java`, `AndroidManifest.xml`, 프레임워크의 `core/res/res/values`(`colors.xml`, `config.xml`, `styles_device_defaults.xml`, `styles_material.xml`, `strings.xml`, `public-final.xml`)와 `alert_dialog_button_bar_material.xml`, `transient_notification.xml`, `toast_frame.xml`, `libs/androidfw`의 `AssetManager2.cpp`, `LoadedArsc.cpp`, `Idmap.cpp`, `ResourceTypes.cpp`, 각 앱의 `overlayable.xml`, PackageInstaller의 `styles.xml`, IntentResolver의 `styles.xml`, `dimens.xml`, `bottomsheet_background.xml`, `chooser_grid_scrollable_preview.xml`.
- R20에서 받은 APK의 `aapt2 dump resources` 출력(`.scratch/list-bottom-margin/a17/res17.txt`는 Settings 17, `apks/PackageInstaller_res.txt`는 PackageInstaller 16)과 APK 목록.

### 방법
- 각 파일을 고정 커밋에서 `?format=TEXT`로 받아 `.scratch/system-ui/`에 두고 16과 17을 비교했다. 오버레이가 바꾸는 리소스의 이름과 구성(qualifier)을 소스와 덤프에서 맞춰 보았다. 이 PC에서는 이미지를 만들거나 실행하지 않았다.

### 찾은 것
- **오버레이 값이 쓰이는 조건**: `AssetManager2::FindEntry`는 오버레이 값의 구성이 대상 값의 구성과 같거나 더 나을 때만 오버레이 값을 쓴다. SettingsLib의 그림과 값은 `-v36`, `-v31` 폴더에 있다. Settings 17은 minSdk 때문에 덤프의 구성이 `()`로 나오지만 16은 확인하지 못했다. 그래서 오버레이도 같은 `-v36`, `-v31` 폴더에 둔다. 이전 Settings 오버레이의 뒤로 아이콘은 `drawable`에 있었으므로, 대상이 `v36`으로 남은 빌드에서는 밝은 테마에서 쓰이지 않았을 수 있다. 같은 이유로 `drawable-v36`으로 옮겼다.
- **오버레이 안의 참조**: 오버레이 패키지는 동적 패키지로 올라간다(`LoadedArsc.cpp`의 `PROPERTY_OVERLAY | PROPERTY_DYNAMIC`). 그래서 리소스 맵에 없는 오버레이 자신의 리소스(검색 아이콘 등)는 오버레이 안에서 풀린다. 리소스 맵에 있는 id와 속성은 `OverlayDynamicRefTable`이 대상의 id로 바꾸고, `ResXMLParser::getAttributeNameResID`도 이 표를 거치므로 오버레이 레이아웃의 `app:` 속성 이름도 대상의 속성이 된다. 리소스 맵이 없는 프레임워크 오버레이는 이름으로만 맞추므로, 그 레이아웃은 프레임워크의 id를 `@*android:id/...`로 직접 쓴다.
- **큰 제목**: SettingsLib의 펼침형 툴바 레이아웃 두 개(`settingslib_expressive_collapsing_toolbar_content_layout`, `settingslib_expressive_support_toolbar_layout`)는 레이아웃 안에서 `app:expanded="false"`라고 쓴다. 16과 17이 같다. 그래서 하위 화면은 큰 제목을 접은 채 연다. 이 속성은 스타일이나 값이 아니고, 레이아웃이 SettingsLib의 다른 레이아웃을 `include`하므로 오버레이로 바꾸려면 그 레이아웃들까지 대신해야 한다. 툴바 높이는 `settingslib_toolbar_layout_height`(179 dp, `values-v31`)이고 툴바는 `?attr/actionBarSize`(AppCompat의 56 dp, 가로 창 48 dp)다.
- **설정 첫 화면**: `settings_homepage_container`에는 제목이 없고 앱 바에 검색창만 있다. 16과 17이 같다. `SettingsHomepageActivity`는 `screenSize`를 스스로 처리하므로 창 크기가 바뀌어도 레이아웃을 다시 만들지 않는다. 코드가 id로 찾는 뷰는 `app_bar_container`, `search_action_bar`, `search_bar_title`, `suggestion_content`, `contextual_cards_content`, `main_content`, `homepage_container`, `main_content_scrollable_container`, `settings_homepage_container`다.
- **카드**: SettingsLib의 그룹 배경은 바깥 모서리를 `settingslib_preference_corner_radius`(20 dp)로, 안쪽 모서리를 4 dp로 둥글게 하고, `MarginItemDecoration`이 행 아래에 2 dp(`settingslib_expressive_space_extrasmall1`)를 둔다. 그 2 dp 값은 다른 여백에도 쓰이므로 바꾸지 않았다.
- **색**: SettingsLib과 Material 3 앱의 색은 프레임워크의 `system_*_light`, `system_*_dark` 값으로 간다. DSA 게스트에는 SystemUI의 테마 서비스가 없으므로 이 기본값이 모든 앱의 색이다. 오버레이가 바꾸는 이름은 모두 16과 17의 `colors.xml`에 있다.
- **겹쳐 쓸 수 있는 것**: DocumentsUI, PermissionController, IntentResolver는 `<overlayable>`을 선언한다. DocumentsUI는 `force_material3`과 몇 가지 색과 글꼴 이름만, PermissionController는 대화상자 스타일과 설정 값만, IntentResolver는 몇 가지 크기와 색만 연다. PermissionController의 목록 배경(SettingsLib의 `settingslib_round_background*`)은 열려 있지 않다. Settings와 PackageInstaller는 선언하지 않는다. 공유 시트의 제목은 프레임워크의 `TextAppearance.DeviceDefault.WindowTitle`이고 시트의 모서리는 `config_bottomDialogCornerRadius`(16 dp)다.
- **토스트**: Android 11부터 R 이상을 대상으로 하는 앱의 글자 토스트는 `NotificationManagerService`가 `StatusBarManagerService.showToast`로 SystemUI에 그리게 한다. DSA 게스트에는 SystemUI가 없으므로 이 토스트는 어디에도 그려지지 않는다(소스로 확인, 기기에서 확인하지 않음). 앱이 스스로 그리는 토스트(Q 이하 대상, `transient_notification`)만 오버레이가 바꿀 수 있다.
- **글꼴**: 이미지의 `sans-serif`는 `external/roboto-fonts`의 Roboto(가변 글꼴, 3.005)이고, 저장소의 NOTICE는 Apache-2.0이다. 17의 저장소에는 `MODULE_LICENSE_OFL`도 있다. 굵기 700은 `textStyle="bold"`로 쓸 수 있다.

### 결정
- 오버레이 여섯 개를 `/vendor/overlay`에 정적으로 둔다: 프레임워크(색, 대화상자와 시트의 모서리, 나란한 버튼, 토스트, 굵은 제목), Settings(카드, 큰 제목, 첫 화면), DocumentsUI(`force_material3`), PermissionController(권한 대화상자의 굵은 제목), PackageInstaller(굵은 제목), IntentResolver(공유 시트의 제목 크기). 모두 이미지의 framework-res로 만들고, 만들기 전에 대상 앱의 리소스 표에 바꿀 이름이 다 있는지 확인한다. 두 리소스 표의 SHA-256을 오버레이의 `assets/dsa-overlay.json`에 남기고, `check-image.py`가 이미지의 표와 비교한다. 서명이 바꾸지 않는 것은 APK가 아니라 리소스 표이므로 표의 해시를 쓴다.
- 오버레이로 안 되는 것은 앱 안에서 고친다. Settings.apk의 컴파일된 툴바 레이아웃 두 개는 `expanded`를 테마 값(`?attr/expanded`)으로 바꾼다. 앱 바의 테마 오버레이 `SettingsLibTheme.CollapsingToolbar.Expressive`(Material 테마 위의 빈 스타일)를 Settings 오버레이가 `values-v36`에서는 거짓, `values-h600dp-v36`에서는 참으로 대신한다. 그래서 낮은 창은 처음부터 접히고 높은 창은 펼쳐 연다. 툴바 영역을 툴바와 같은 높이로 줄이는 방법은 쓰지 않았다. Material의 `CollapsingToolbarLayout`은 펼침 비율을 `|offset| / (높이 - 최소 높이)`로 구하므로 두 값이 같으면 0으로 나누게 된다. 첫 화면은 `layout-h600dp` 레이아웃에만 큰 제목을 둔다. PermissionController.apk의 컴파일된 그룹 배경은 Settings 카드와 같은 모서리(바깥 26 dp, 안쪽 0)로 바꾼다.
- 오버레이가 대상의 리소스를 이름으로 가리키려면 그 리소스를 리소스 맵에 넣어야 하고, 그러면 대상의 값을 대신하게 된다. 그래서 Material 테마(`Theme.MaterialComponents.DayNight`)는 시계(`-watch`)에서만 맞는 대역으로 맵에 넣는다. 오버레이에 맞는 구성이 없으면 `FindEntry`가 대상의 값을 그대로 쓰므로, 참조는 Settings의 스타일로 풀린다.
- 행 사이의 2 dp는 그대로 두고 안쪽 모서리를 각지게 해서, 그 틈이 카드 안의 구분선이 되게 한다.
- 수용 기준 2와 3은 이미지 테스트의 Android 17 x86_64 부팅 확인에서 본다(`dsa/host/platform/linux/tests/system_screens.rs`). 개발자 모드의 adb로 `uiautomator dump`와 `screencap`을 읽는다.

### 대가와 남은 것
- 첫 화면은 창 크기가 바뀌어도 레이아웃을 다시 만들지 않으므로, 600 dp를 넘나들게 크기를 바꾼 창은 화면을 다시 열 때까지 처음의 상태를 지킨다.
- DocumentsUI의 레이아웃은 겹쳐 쓸 수 없으므로 파일 선택기에는 큰 제목 영역이 없다. 색과 Material 3의 둥근 모양만 바뀐다. 큰 제목을 넣으려면 DocumentsUI의 레이아웃 코드를 고쳐야 한다.
- PermissionController는 앱 바의 테마 오버레이를 `<overlayable>`에 열지 않아, 권한 화면은 SettingsLib대로 큰 제목을 접은 채 연다. 높은 창에서 펼쳐 열려면 그 앱의 레이아웃이나 코드를 고쳐야 한다.
- 소유자에게 따로 보고할 것: SystemUI가 없는 게스트에서는 앱의 글자 토스트가 보이지 않는다. 호스트 알림 등으로 보여 주는 것은 새 요구사항이다.

### 확인하지 못한 것
- 이미지를 만들거나 부팅하지 않았다. aapt2가 오버레이를 받아들이는지, idmap2가 각 오버레이를 받아들이는지(`adb shell cmd overlay list`), 오버레이 레이아웃의 `app:` 속성이 실제로 Settings의 속성으로 읽히는지, 컴파일된 레이아웃을 고친 Settings와 PermissionController가 그대로 뜨는지는 develop의 이미지 테스트가 처음 확인한다.
- Android 16 Settings의 덤프는 없었다. 16의 이름은 소스로만 확인했고, 이미지 빌드가 대상 리소스 표로 다시 확인한다.
- 밝은 테마와 어두운 테마에서 Settings, 파일 선택기, 권한 대화상자가 같은 규칙을 따르는지는 수동 확인이다(FR-34의 네 번째 수용 기준).

## R24. v0.1.9 실기 시험의 실패: GPU 리셋 뒤 프레임워크 재시작, 남은 창, 두 번째 태스크의 강제 종료 (NFR-5, FR-3, FR-14, FR-21, FR-26)

### 확인한 자료
- 소유자 PC(Samsung Galaxy Book NT960XGP, Intel Arc, Windows 11 x64)에 설치한 DSA 0.1.9와 Android 17 이미지 `17.0.0-16373615-9`(고친 프레임워크 `17-1cda471fb363`). 2026-11-08 20:49 ~ 21:03 KST.
- `dsa logs collect`가 만든 묶음(`.scratch/rel-0.1.9/diag.zip`): `android/logcat.txt`(2,109줄), `android/dumpsys.txt`, `host/dsa-wm.log`, `host/dsa-hostd.log`, `host/dsa-app-window.log`, `host/android-console.log`(게스트 콘솔, 6,078줄).
- 창 캡처(`after-tap.png`, `click1..5.png`, `tap1..5.png`), 시험 스크립트(`click.ps1`, `uia.ps1`, `toast.ps1`), 설치 로그(`install2.log`), 응용 프로그램과 시스템 이벤트 로그, `%LOCALAPPDATA%\CrashDumps`.
- Android 17 `frameworks/native` `ae266dc`의 RenderEngine 소스, gfxstream `d047a57`의 `DeviceOpTracker.cpp`와 `FrameBuffer.cpp`, WiX 5.0.2의 부트스트래퍼 API.

### 방법
- 게스트 시각과 호스트 시각을 맞추었다. logcat의 이벤트 37001은 벽시계와 부팅 뒤 시간을 함께 적으므로(11:59:54.524에 409.844 s), 게스트 콘솔의 부팅 뒤 시간을 벽시계로 바꿀 수 있다. 호스트의 오디오 닫힘(11:59:55.489)이 audioserver 재시작과 같은 순간이라 보고, 게스트 시계가 호스트보다 약 1.7 ~ 2.0 s 빠르다고 잡았다. 설정 앱 강제 종료(게스트 11:54:21.952)와 호스트의 VkDevice 정리(11:54:19.950)도 같은 차이를 준다.
- 이 PC에서는 아무것도 실행하지 않았다. 로그와 이벤트 로그만 읽었다.

### 찾은 것
- **GPU 리셋과 프레임워크 재시작**: 렌더러가 11:59:54.692 UTC에 게스트의 `vkQueueSubmit`에서 `VK_ERROR_DEVICE_LOST`를 받았다. 렌더러는 끝나지 않았고(R18의 패치 0015), 장치 넷에 손실을 알리고 두 장치를 정리했다. 시스템 이벤트 로그에 TDR 기록(4101)은 없다(R18과 같다). 게스트 콘솔에서는 약 0.5 s 뒤(부팅 뒤 412.46 s) `Service 'surfaceflinger' (pid 677) received SIGSEGV`이고, init이 `onrestart`로 zygote를 끝내 프레임워크 전체가 다시 시작했다. abort가 아니라 SIGSEGV이므로 패치가 경고로 바꾼 `LOG_ALWAYS_FATAL` 자리는 아니다. 그 직전(412.165 s) 셸(uid 2000)의 프로세스 하나가 gralloc을 처음 열었다.
- **스택이 없는 이유**: SurfaceFlinger의 tombstone은 묶음에 없다. 세 가지가 겹쳤다. (1) 에이전트가 dropbox를 읽지 못했다(`Neither user 10068 nor current process has android.permission.READ_DROPBOX_DATA`). Android 11부터 dropbox는 READ_LOGS와 사용 통계 말고도 이 권한을 요구한다. (2) Cuttlefish의 센서 멀티 HAL이 5초마다 abort하며 crash 버퍼와 main 버퍼를 약 70줄씩 채웠다. logcat의 main 버퍼는 12:03 무렵의 것만 남았고, 그 tombstone도 몇 분 만에 순번이 돌아 덮였다. (3) 시험 스크립트의 `logcat -c`. 그래서 SurfaceFlinger가 어디서 죽었는지는 이 묶음으로 알 수 없다.
- **센서 HAL**: Android 17의 Cuttlefish는 센서 멀티 HAL을 APEX가 아니라 vendor 파일로 둔다(`bin/hw/android.hardware.sensors-service.multihal`, `etc/init/android.hardware.sensors-service-multihal.rc`, `lib64/hw/android.hardware.sensors@2.1-impl.cuttlefish.so`, `etc/sensors/hals.conf`). 이미지는 APEX 둘과 VINTF 조각만 지웠으므로 init이 이 서비스를 계속 띄웠고, 하위 HAL이 Cuttlefish의 센서 제어 소켓을 찾지 못해 abort했다(`Could not connect to sensors control: No such file or directory`).
- **렌더러의 경고**: `had a waitable pending for -21043 milliseconds. Leaking object.`는 GPU 작업이 멈춘 표시가 아니다. `DeviceOpTracker::PollAndProcessGarbage`는 장치를 쓸 때만 쓰레기를 보고, 5초보다 오래된 것은 끝났는지와 상관없이 새게 두며, 나이를 `timepoint - now`로 거꾸로 적는다. 그 장치를 한동안 아무도 쓰지 않으면 매번 펜스나 세마포어 하나가 샌다. `openColorBuffer cb handle -2147483645 not found`도 오류가 아니다. 게스트의 gralloc(패치 0002)은 공유 ColorBuffer(맨 위 비트가 켜진 핸들)를 먼저 열어 보고, 없으면 만든다. 그 첫 열기마다 렌더러가 오류를 적었다. 둘 다 장치 손실의 원인으로 보이지 않는다.
- **남은 창(FR-3)**: 프레임워크가 다시 시작하자 에이전트도 끝났다(`dsa-hostd.log`의 "the agent is gone", 이어서 "connected"). 서비스의 태스크 표는 에이전트의 보고로만 바뀌는데, 연결이 끊긴 것은 태스크를 지우지 않았다. 그래서 설정 창은 마지막 프레임(가동 시간 06:52)을 보였고, `dsa tasks`는 태스크 6을 초점이 있는 채로 보였으며, 클릭과 접근성 동작은 없는 태스크로 갔다.
- **UI Automation Invoke(FR-18)**: Invoke 일곱 번은 모두 21:01:59 ~ 21:02:34에, 곧 재시작 뒤에 있었다. 앱 창 로그가 일곱 번 모두 "the screen changed; the node is no longer there"라고 적었다. 끝난 태스크의 노드에 보낸 동작이다. 재시작 전의 Invoke는 없었으므로 Invoke 자체의 결함은 찾지 못했다. 한 번의 Invoke에 결과 줄이 두 번씩 남은 것은 따로 살펴야 한다.
- **두 번째 설정 태스크와 강제 종료**: `am start -a android.settings.DEVICE_INFO_SETTINGS`가 태스크 5를 만들고(게스트 11:54:21.795), 115 ms 뒤 에이전트가 태스크 4와 5를 `removeTask`로 지웠다(`remove-by-pid#2440`). 에이전트가 태스크를 지우는 길은 호스트의 `CloseTask`뿐이다. 이어서 에이전트의 닫힌 앱 정리(`ClosedApps`)가 창이 하나도 없게 된 설정 앱을 끝냈다(`stop com.android.settings due to from pid 2440`). 그 정리는 설계대로다. 원인은 그 앞이다. 창 관리자는 앱 창 프로그램과의 연결이 끝나면(`ManagerEvent::Gone`) 그 창들을 사용자가 닫은 것으로 치고 `CloseTask`를 보낸다. 같은 순간 창 관리자는 태스크 4의 프레임을 500 ms 안에 보이지 못했다고 적었고, 앱 창 로그와 창 관리자 로그에는 연결이 끝난 까닭이 없다. 이벤트 로그와 CrashDumps에 앱 창 프로그램의 충돌 기록은 없다. 곧 프로그램은 조용히 끝났다. 조용히 끝나는 길은 셋이다. 창 관리자가 연결을 닫은 경우(창 관리자의 수신 오류가 로그 없이 연결을 끝낸다), 앱 창의 UI 스레드에 깨우는 메시지를 넣지 못한 경우(그것을 UI 스레드가 끝난 것으로 쳤다), 프레임을 보였다는 답을 보내지 못한 경우다. 어느 쪽이었는지는 로그로 가를 수 없다.
- **20:54:52의 "kill all background"**: 창이 하나도 없게 된 30초 뒤 에이전트가 Doze에 들어가며 `deviceidle force-idle deep`과 `activity kill-all`을 불렀다(이벤트 34000의 `s:shell`, 34009 `deep`). 캐시된 프로세스만 끝나는 설계된 동작이다(INTENT D7).
- **이미지 배포의 판(FR-14)**: 릴리스 워크플로는 `inputs.json`의 수동 `revision`으로 판을 정하고, 같은 태그가 있으면 `gh release upload --clobber`로 파일을 바꿔 넣었다. 그래서 새 이미지가 `-9`로 다시 나갔다.
- **설치 프로그램의 판**: CI의 실제 설치 프로그램은 게시된 최신 판보다 한 단계 높은 판(0.1.9)으로 만들고, 같은 판의 릴리스가 그것을 대신한다고 보았다. 그런데 Burn이 MSI를 `REINSTALLMODE="muso"`로 실행했다(`install2.log`). 명령줄의 속성이 패키지의 `REINSTALLMODE=amus`보다 우선하고, `o`는 판이 같은 파일을 두므로 릴리스가 CI 빌드의 파일을 거의 바꾸지 않았다. WiX 5의 부트스트래퍼 API는 `OnPlanMsiPackage`의 `FileVersioning`으로 이 글자를 고르게 한다.
- **토스트(R23)**: logcat과 dumpsys에 토스트의 흔적은 없다. `NotificationManagerService`는 SystemUI가 없을 때 글자 토스트를 버린다고 따로 적지 않는다. 확인하려면 탭 바로 뒤에 `adb shell dumpsys notification`의 `Toast Queue`를 읽는다. 큐에 기록이 있고 `dumpsys window windows`에 토스트 창이 없으면, 기록은 받았지만 아무도 그리지 않은 것이다.

### 결정
- 이미지: 센서 HAL을 DSA 것으로 바꿀 때 plain 파일로 둔 다른 센서 서비스와 하위 HAL 목록도 지운다(`make-image.py`의 `cuttlefish_sensors_file`). `check-image.py`는 그런 파일이 남은 이미지를 거절한다.
- 에이전트: `READ_DROPBOX_DATA`를 요청하고 특권 권한 목록에 넣어, 묶음에 tombstone이 들어가게 한다.
- 렌더러: 패치 0016은 끝난 쓰레기를 지우고 아직 끝나지 않은 것만 새게 두며 나이를 바로 적는다. 패치 0017은 공유 ColorBuffer의 첫 열기 실패를 오류로 적지 않는다.
- 서비스: 게스트가 에이전트가 끝났다고 알리면 그 에이전트가 보고한 태스크를 모두 지운다. 그래서 창이 닫히고, 새 프레임워크가 같은 번호를 다시 주어도 새 태스크로 다룬다. 에이전트만 끝난 경우에는 다음 에이전트가 태스크를 다시 보고해 창이 다시 열린다.
- 앱 창 프로그램: 깨우는 메시지를 넣지 못해도 명령은 채널에 남고 UI 스레드의 타이머(200 ms)가 가져간다. UI 스레드가 끝났을 때만 프로그램이 끝난다. 앱 창과 창 관리자는 연결이 끝난 까닭과, 창이 열린 채 프로그램이 끝나 태스크가 끝나는 일을 로그에 남긴다.
- 이미지 배포: 워크플로가 게시된 태그를 읽어 같은 빌드와 아키텍처의 가장 높은 판보다 하나 높은 판을 고르고(`make-image.py --next-revision`), 그 판으로 이미지를 만든다(`--revision`). 이미 있는 태그에는 올리지 않고 실패한다. 이미지의 빌드 번호가 AOSP 빌드 번호 × 100 + 판이므로 판은 99까지다.
- 설치 프로그램: 설치 화면(`SetupApplication.OnPlanMsiPackage`)이 `FileVersioning`을 `All`로 정해, Burn이 `a`를 넣는다. 설치 시험이 Burn 로그에서 이를 확인한다.

### 대가와 남은 것
- 에이전트만 끝나고 앱은 살아 있는 경우에도 창이 닫혔다가 다시 열린다. 그 경우는 드물고, 끝난 태스크의 창이 남는 것보다 낫다고 보았다.
- 앱 창 프로그램이 창을 연 채 끝나면 지금도 그 태스크가 끝난다. 작업 관리자의 "프로세스 끝내기"와 충돌을 가를 수 없기 때문이다. 충돌로 끝난 경우에는 창 프로그램을 다시 띄워 창을 다시 여는 쪽이 낫지만, 이것은 동작을 바꾸는 일이라 소유자의 결정이 필요하다.

### 확인하지 못한 것
- SurfaceFlinger가 GPU 리셋 뒤 SIGSEGV로 끝난 자리. 다음 실기 시험의 묶음에 tombstone이 들어와야 고칠 곳을 알 수 있다. 그 전에는 D24의 수동 기준(GPU 리셋 뒤 system_server의 pid가 바뀌지 않는다)을 충족한다고 말할 수 없다.
- 두 번째 태스크를 열 때 앱 창 프로그램이 끝난 정확한 길. 이번 수정 뒤의 로그가 그 까닭을 적는다.
- 리셋 자체의 원인(Intel 드라이버의 엔진 리셋을 부른 게스트 명령). 렌더러 로그는 어느 게스트 프로세스의 제출인지 적지 않는다.
- 위의 모든 수정은 이 PC에서 빌드하거나 시험하지 않았다. `dsa-host-core`와 `dsa-host-windows`의 clippy와 설치 화면의 `dotnet build`만 `scripts/compile-check.ps1`로 확인했고, 시험과 이미지, 렌더러, 에이전트 빌드는 develop의 CI가 처음 확인한다.

## R25. 앱의 소리와 MediaCodec이 안 되던 까닭: memfd의 MLS 제약과 코덱 저장소의 설정 거절 (PR-4, FR-26, 이슈 #387)

### 확인한 자료
- 이슈 #387: Android 17 이미지 `17.0.0-16373615-9`, 게스트 커널 6.18.54.1-dsa(KernelSU를 넣은 커널과 원래 커널 모두). 첨부한 `dsa-wm.log`(1,467줄)는 열 번 남짓 연 오디오 스트림이 모두 `0 frames received`로 닫혔음을 보인다.
- 소유자 PC의 0.1.9 묶음(`.scratch/rel-0.1.9/diag.zip`)의 `host/dsa-wm.log`, `android/logcat.txt`, `host/android-console.log`. 같은 증상(`audio stream primary closed (the guest closed it): 0 frames received`)과, 부팅마다 남는 `scontext=u:r:hal_sensors_default:s0 tcontext=u:object_r:system_server_tmpfs:s0 tclass=file` 거절이 있다.
- Android 16 입력(`aosp_cf_x86_64_only_phone` 15581820)의 `plat_sepolicy.cil`, `vendor_sepolicy.cil`, `ashmem_compat_memfd_context.txt`.
- AOSP 소스(gitiles, 2026-11-08): `system/sepolicy`의 `android16-release`, `android16-qpr2-release`, `android17-release`(`private/mls`, `te_macros`, `app.te`, `audioserver.te`, `mediaserver.te`, `mediaextractor.te`, `system_server.te`, `hal_sensors.te`, `hal_codec2.te`), `system/core`의 `libcutils/ashmem-dev.cpp`(세 갈래), `frameworks/av` `android16-qpr2-release`의 `C2Store.cpp`, `C2DmaBufAllocator.cpp`, `CCodecBufferChannel.cpp`, `CCodec.cpp`, `hal/client/client.cpp`, `hal/aidl/Configurable.cpp`, `module/codecserviceregistrant/CodecServiceRegistrant.cpp`.

### 방법
- 정책은 컴파일한 CIL을 읽어 규칙과 MLS 제약을 대조했다. 코덱은 이슈의 로그 줄을 소스에서 찾아 거꾸로 따라갔다.
- 이 PC에서는 아무것도 실행하지 않았다. 이미지도 정책도 빌드하지 않았다.

### 찾은 것
- **소리**: DSA의 커널에는 ashmem이 없어 이미지가 `sys.use_memfd=true`로 공유 메모리를 memfd로 만든다. memfd는 만든 도메인의 tmpfs 타입을 받는다(audioserver가 만든 트랙의 컨트롤 블록은 `audioserver_tmpfs`, 수준 `s0`). 앱은 자기 MLS 수준(`s0:c149,c256,c512,c768` 같은 범주)에서 돈다. `private/mls`는 수준이 낮은 파일에 쓰기를 `l1 eq l2`이거나 대상 타입이 `mlstrustedobject`일 때만 허락한다. binder는 옮기는 fd마다 받는 쪽의 read와 write를 확인하므로, 컨트롤 블록을 실은 응답 전체가 거절되고 `AudioTrack`은 "Could not get control block"(-20, NO_INIT)으로 실패했다. 플랫폼 정책은 `appdomain audioserver_tmpfs`를 이미 허락하지만 제약은 allow 규칙으로 풀리지 않는다. 신고자의 KernelSU 규칙이 듣지 않은 까닭이다. `system_server_tmpfs`는 같은 까닭으로 `mlstrustedobject`이고, `audioserver_tmpfs`, `mediaserver_tmpfs`, `mediaextractor_tmpfs`, `cameraserver_tmpfs`는 아니다. 휴대전화는 `/dev/ashmem`(`ashmem_device`, `mlstrustedobject`)을 쓰므로 이 길을 지나지 않는다.
- **타입 규칙**: Android 17은 memfd의 공유를 `memfd_file` 클래스 규칙으로만 준다(`appdomain audioserver:memfd_file`, `mediaserver mediaextractor:memfd_file`, `system_server audioserver:memfd_file`, `audioserver appdomain:memfd_file` 들). 이 클래스는 커널과 정책의 `memfd_class` 정책 능력이 함께 있어야 쓰이고, Android 17의 `policy_capabilities`에는 그것이 없다. 신고자의 거절도 `file` 클래스의 `audioserver_tmpfs`다. 그래서 같은 짝의 `file` 규칙이 tmpfs 타입에 없다. 신고자의 `mediaserver mediaextractor_tmpfs` 거절이 그 하나다.
- **libcutils**: Android 16 QPR2의 정책에는 모든 도메인이 쓸 수 있는 `ashmem_compat_memfd` 타입과 `tmpfs_domain`의 `relabelfrom`이 있지만, `android16-qpr2-release`와 `android17-release`의 `ashmem-dev.cpp`는 memfd의 레이블을 바꾸지 않는다. Android 17의 libcutils는 `sys.use_memfd`가 없으면 `memfd_class` 능력, vendor API 202604 이상, 앱의 target SDK 37 이상일 때만 memfd를 쓴다. DSA는 그 속성을 켜므로 모든 프로세스가 memfd를 쓴다.
- **센서**: 프레임워크의 센서 서비스(system_server)가 이벤트 큐와 웨이크 락 큐를 memfd로 만들어 `ISensors.initialize`로 넘긴다. Android 16과 17의 정책에는 `hal_sensors`가 system_server의 memfd를 쓰는 규칙이 없다(17은 앱과 `hal_allocator`의 `memfd_file`만 준다). 그래서 부팅마다 그 호출이 거절되었고 DSA의 센서 HAL은 이벤트를 쓸 큐를 받지 못했다. 둘 다 `s0`이므로 MLS 제약은 없다.
- **MediaCodec**: `CCodec::allocate`는 `IComponentStore/default`가 있으면 그것을 앱 프로세스의 선호 저장소로 정한다(`media.swcodec`의 `CodecServiceRegistrant`도 같다). Cuttlefish에는 그런 저장소가 없었고, DSA의 `dsa-codec2`가 처음으로 그 이름을 쓴다. `C2DmaBufAllocator`는 선호 저장소에 `C2StoreDmaBufUsageInfo`의 지원 값을 묻고, 답이 `C2_OK`이면 할당마다 그 매개변수를 `config`하는 매퍼를 단다. 그 `config`가 실패하면 할당이 실패한다(`newLinearAllocation`의 `if (ret && ret != C2_NO_INIT) return ret`). `dsa-codec2`의 저장소는 `querySupportedValues`에 필드마다 `C2_BAD_INDEX`를 적고 전체로는 `C2_OK`를 돌려주었으며(플랫폼의 `C2InterfaceHelper`도 그렇게 답한다), `config`는 모두 `C2_BAD_INDEX`로 거절했다. 그래서 입력 기본 풀의 `fetchLinearBlock`이 모두 실패했고 `start: cannot allocate memory at all`, NO_MEMORY가 났다. SELinux와 상관이 없어 permissive에서도 같았고, 소프트웨어 디코더도 같은 앱 쪽 할당을 쓰므로 함께 실패했다. 플랫폼의 저장소(`C2PlatformComponentStore`)는 `ion-usage`와 `dmabuf-usage`를 선언하고 답한다.
- **코덱의 memfd**: `mediaswcodec`과 `mediacodec`(dsa-codec2)에는 tmpfs 타입이 없어 그 memfd(버퍼 풀 연결의 큐)가 plain `tmpfs`였다. 앱은 그것을 매핑할 규칙도 없고, MLS 수준 때문에 쓸 수도 없다. Android 17은 `appdomain hal_codec2:memfd_file`을 준다.

### 결정
- `memory.cil`: `audioserver_tmpfs`, `mediaextractor_tmpfs`, `mediaserver_tmpfs`, `cameraserver_tmpfs`를 `mlstrustedobject`에 넣고, Android 17의 `memfd_file` 규칙과 같은 짝을 tmpfs 타입의 `file` 규칙으로 준다. 코덱 서비스의 memfd에는 `dsa_codec2_tmpfs`를 주고(`mediaswcodec`, `mediacodec`의 타입 전이), `hal_codec2_client`가 쓰게 하며 `mlstrustedobject`로 둔다.
- `sensors.cil`: `hal_sensors_server`가 system_server와 앱의 memfd를 쓰게 한다.
- `dsa-codec2`: 저장소가 플랫폼 저장소와 같은 `ion-usage`, `dmabuf-usage`를 선언하고 답한다(`system` 힙, 플래그 없음). 게스트 커널의 DMA-BUF 힙은 `system` 하나다.
- 시험: `check-image.py`가 공유 규칙과 `mlstrustedobject` 타입을 확인한다(`make-image.py`의 `MEMFD_SHARING`, `MEMFD_SHARING_SENSORS`). 이미지 워크플로는 보통 앱인 미디어 프로브(`dsa/guest/media-probe/`)를 SDK 도구로 빌드하고, 부팅한 이미지에서 SELinux enforcing으로 실행한다(`dsa/host/platform/linux/tests/media.rs`). 스트리밍 트랙과 정적 트랙이 1초 음을 받아 재생해야 하고, 1초의 H.264와 AAC를 인코딩해 게스트가 나열한 모든 디코더로 디코딩해야 하며, 게스트 로그에 미디어 서비스의 공유 메모리 거절이 없어야 한다.

### 대가와 남은 것
- `mlstrustedobject`는 그 타입의 파일에 대해 앱 사이의 MLS 분리를 푼다. 이 타입들의 파일은 서비스가 만들어 binder로 건넨 memfd뿐이고, 경로로 열 수 없으며, 받을 수 있는 쪽은 여전히 타입 규칙이 정한다. `system_server_tmpfs`와 같은 처지다.
- 전원 HAL(ADPF)과 ContextHub HAL이 앱에 주는 memfd도 Android 17은 `memfd_file`로 허락한다. DSA의 그 HAL들에는 tmpfs 타입이 없어 이번에 다루지 않았다. 앱이 그 큐를 받지 못하면 그 기능만 binder로 물러나는지 확인해야 한다.
- 시험은 게스트가 재생한 위치까지 확인하고, 호스트가 프레임을 받았는지는 확인하지 않는다. 호스트의 오디오 포트는 스트림이 닫힐 때에만 받은 프레임 수를 적는다.

### 확인하지 못한 것
- 이 PC에서는 이미지, 정책(secilc), dsa-codec2, 미디어 프로브를 빌드하지 않았다. Android 17 입력의 `plat_sepolicy.cil`은 받아 두지 않아, 새 규칙이 Android 17의 neverallow에 걸리지 않는지는 Android 16 정책으로만 확인했다. develop의 이미지 워크플로가 처음 확인한다.
- `media.swcodec`의 `querySupportedValues -- transaction failed`와 binder의 `-22`(없는 핸들로의 호출)는 소스로 원인을 찾지 못했다. 앱 쪽 실패와는 따로 일어난다(그 실패가 있으면 media.swcodec은 매퍼 없이 기본 힙을 쓴다). 수정한 이미지의 로그로 다시 본다.
- 신고자의 이미지(KernelSU, LiteGapps)에서의 결과와, 호스트의 GPU 디코더(`c2.dsa.avc.decoder`)로의 디코딩. CI의 호스트는 GPU 디코더를 내놓지 않을 수 있다.

## R26. v0.2.1에서 메모리가 늘고 안드로이드가 느려진 까닭 (NFR-1, NFR-2, NFR-3)

**기준.** 소유자 PC(Windows 11 Pro 26300, x64, Intel Arc, 32 GiB, 호스트 가용 메모리 2.4-3.2 GiB로 "short" 상태가 잦다), Android 17 이미지, v0.1.9부터 v0.2.1까지 같은 PC의 로그를 읽었다. 이 조사에서 빌드하거나 실행하지 않았고, 설치된 DSA와 바탕 화면은 건드리지 않았다. 자료는 `%LOCALAPPDATA%\DSA\logs\dsa-wm.log`(렌더러의 메모리 줄), `C:\ProgramData\DSA\logs\dsa-hostd.log`(서비스의 `memory:` 줄), 사용자별 안드로이드 로그(`dsa-channeld`의 회수 줄과 init의 명령 시간)다. CI의 Image test와 Installer test는 실제 메모리를 재지 않는다. Installer test에는 부팅한 안드로이드가 없어 게스트 값이 없고, 설정 앱 사진의 메모리 값은 가짜(`memory-fixture.ps1`)다. 그래서 v0.1.10과의 비교는 이 로그로만 한다.

**소유자가 잰 값 (v0.2.1, Google Play 설치, 앱 한두 개, 최근 ARM 앱).** 호스트 2446 MiB: vm 상주 1742/private 1787, 렌더러(dsa-gfx) 598/814, 창 관리자 35, 앱 창 71. 게스트 2975 MiB 중 1655 사용: 페이지 캐시 741, 앱 메모리 476, 커널과 나머지 403. "VM이 안드로이드가 쓰는 것 밖에 잡은 몫 0 MiB"이므로 빈 페이지 보고는 제대로 돈다. 문제는 게스트가 쓰는 양 자체다.

### 로그로 본 비교

| 시점 | 호스트 합 | vm | 렌더러 | 게스트 사용 | 캐시 | 앱 | PSS |
|---|---|---|---|---|---|---|---|
| v0.1.9, 앱 창 1개 | 1249 | 923 | 223 | 862 | 277 | 230 | 278 |
| v0.2.1, Google 앱 없음, 창 0개, 부팅 직후 | 877-944 | 744-806 | 102-104 | 820-826 | 170-289 | 217-233 | 270-291 |
| v0.2.1, Google 앱 있음, 창 0개, 부팅 직후 | 980-1218 | 849-977 | 103-217 | 1387-1838 | 460-936 | 348-399 | 685-702 |
| v0.2.1, Google 앱 있음, 창 1개(YouTube) | 1677-2347 | 987-1578 | 386-793 | 1028-1496 | 239-498 | 293-569 | 703-930 |
| v0.2.1, Google 앱 있음, YouTube를 닫고 1-2분 | 777-1019 | 688-932 | 61-62 | 653-975 | 137-325 | 89-182 | 329-338 |

렌더러의 GPU 할당(`dsa-wm.log`의 "GPU-only"): v0.1.9는 앱을 연 동안 66-111 MiB(3-13개, 가장 큰 것 33 MiB), v0.2.1의 YouTube 세션은 567-931 MiB(44-99개, 가장 큰 것 33 MiB)다. 이 수는 렌더러 자신의 장치로 만든 것, 곧 gralloc 버퍼 뒤의 ColorBuffer와 창의 텍스처만 센다(게스트가 디코더 스트림으로 만든 할당은 다른 디스패치를 거쳐 세지 않는다). 창을 닫자 렌더러는 61 MiB로 돌아왔다. 버퍼가 풀리지 않는 누수는 보이지 않는다. 앱이 연 동안의 ColorBuffer(동영상 디코더의 출력 버퍼, 앱의 BufferQueue, 태스크마다 디스플레이 크기의 합성 대상 둘)가 크다.

### 원인별로 나눈 것

1. **DSA의 회귀: 프레임마다 앱의 GPU 메모리 전체를 호스트에서 다시 읽었다 (v0.2.1에서 새로 생김).** guest/mesa 0005(`94262816`, v0.2.0에는 없다)는 게스트가 사본을 두는 호스트 가시 메모리를 coherent로 알리고, 펜스의 첫 성공, `vkWaitSemaphores`, `vkDeviceWaitIdle`마다 그 장치의 모든 사본을 `vkInvalidateMappedMemoryRanges`로 통째로 다시 읽는다. 이 호스트에서 게스트가 보는 메모리 형식은 하나(DEVICE_LOCAL, HOST_VISIBLE, HOST_CACHED)뿐이어서 이미지 메모리까지 모두 사본이 있고, Skia는 프레임마다 펜스를 확인한다. 그래서 앱과 SurfaceFlinger는 프레임마다 자기 GPU 메모리 전체(수십에서 수백 MiB)가 Hyper-V 소켓을 건너오기를 기다렸다. 제출마다 전체 사본의 해시도 다시 계산했다. 측정은 없지만, 코드로 보면 "느려졌다"는 체감의 가장 큰 후보다.
2. **DSA의 기존 비용: 사본을 0으로 채워 만들었다 (patches/0003부터, v0.1.10에도 있다).** 할당마다 `aligned_alloc` 뒤 `memset`으로 사본 전체가 바로 상주했다. 앱의 텍스처, 글리프 아틀라스, 렌더 타깃이 게스트의 앱 메모리(익명)로 한 번 더 잡혔다. CPU가 읽지 않는 메모리다.
3. **Google 앱이 더하는 것.** 같은 v0.2.1에서 창이 없을 때 Google 앱이 있으면 게스트 사용이 약 +560-1010 MiB, PSS가 약 +400 MiB, 캐시가 약 +290-650 MiB다(부팅 직후 1-3분). 앱을 닫고 시간이 지나면 PSS는 약 330 MiB로 내려오지만, Google 앱이 없을 때(270-291)보다 여전히 크다. 게스트의 회수 줄로 보면, 창이 없는 동안에도 내려놓은 캐시가 10-60초 안에 200-440 MiB로 다시 찬다. 그래서 유휴 회수(zram 4단계, `drop_caches`, 압축)가 10-60초마다 되풀이된다. 한 번에 커널 시간 약 0.5-2초다(init 로그의 명령 시간 합). 로그만으로는 무엇이 읽는지 가를 수 없다. Play 스토어와 Play 서비스의 부팅 뒤 작업(업데이트, 동기화)이거나, 내려놓은 캐시를 실행 중인 프로세스가 곧바로 다시 읽는 것이다. 후자라면 정책 쪽(아래 선택지 3)의 문제다.
4. **해상도(밀도).** v0.2.1은 안드로이드를 주 모니터의 밀도로 시작한다(`9b7c92e2`). 그 전에도 태스크는 창이 열리자마자 그 모니터의 밀도를 받았으므로(그때 앱이 다시 시작됐다), 앱이 안정된 뒤의 버퍼 크기는 같다. 버퍼는 창의 물리 픽셀 크기다. 밀도 자체가 버퍼를 6배로 키우지는 않는다. 다만 250% 모니터의 큰 창은 원래 픽셀이 많다. 2560x1536 창의 버퍼 하나가 15 MiB, 태스크의 합성 대상(2880x2880)이 32 MiB다.
5. **화면 캡처의 읽어 오기(guest/mesa 0002, 0006).** CPU가 읽는 버퍼(screencap, ImageReader, SurfaceFlinger의 캡처)에만 붙는다. 앱의 창 버퍼와 합성에는 붙지 않는다. 태스크 스냅숏(창을 숨기거나 닫을 때 시스템이 찍는다)도 SurfaceFlinger 캡처라서 한 장마다 한 번 읽어 오고 그 전에 장치가 쉴 때까지 기다린다. 전환 때의 끊김 후보이지만 상주 메모리는 늘리지 않는다.
6. **페이지 캐시는 Active에서도 줄인다.** 의심과 달리 Dozing에서만 돌려주는 것이 아니다. YouTube 세션 31분 동안 작업 집합 다듬기가 81번(한 번에 66-132 MiB, 목표 256-320 MiB) 돌았다. 측정 순간의 741 MiB는 다듬기 사이에 다시 찬 캐시와 공유 메모리(다듬을 수 없다)를 함께 센 값이다.
7. **VM 크기.** 그 세션들 가운데 한 번은 VM이 3072에서 4608 MiB로 커졌고, 줄이는 요청이 `HcsModifyComputeSystem` 0x800705b4(시간 초과)로 실패해 그 뒤로는 키우지도 줄이지도 않았다. 게스트의 MemTotal은 4383 MiB로 남았다. 이 실패의 원인은 확인하지 못했다.
8. **ARM 번역기.** 기본 번역기 이미지는 부팅마다 loop로 붙지만, ARM 앱이 돌 때에만 그 라이브러리가 캐시에 오른다. 로그로 따로 떼어 볼 수 있는 숫자가 없다.

### 고친 것

guest/mesa 0007(`dsa_shadow_memory.h`): 사본은 커널이 처음 쓸 때에야 내주는 0 페이지(익명 mmap)로 만들고, 앱이 매핑한 할당만 호스트와 맞춘다. 처음 매핑할 때 한 번 다시 읽어 그전에 장치가 쓴 것을 담는다. 앱이 매핑하지 않는 메모리(이미지 등, 앱 GPU 메모리의 대부분)는 보내지도, 다시 읽지도, 건드리지도 않는다. 기대: 프레임마다의 전체 읽어 오기가 매핑된 버퍼(유니폼, 정점, 스테이징, 보통 몇 MiB)로 줄고, GPU를 쓰는 프로세스마다 이미지 메모리만큼의 익명 메모리가 빠진다. 시험: `tests/shadow_memory_test.cpp`(mincore로 상주 페이지를 센다), `test_mapped_memory_patch.py`.

### Google 앱의 비용에 대한 선택지 (4는 구현했고, 3은 먼저 잰다)

1. 그대로 둔다. Google 앱은 사용자의 선택이고, 부팅 직후 PSS 약 +400 MiB, 앱을 닫은 뒤 약 +50 MiB, 캐시 수백 MiB가 그 값이다.
2. Google 앱이 있을 때 캐시 프로세스 상한(지금 8)을 더 낮추거나 Play 서비스의 비상주 프로세스를 제한 대기 버킷에 둔다. 메모리는 줄지만 푸시 알림과 동기화가 늦어질 수 있다.
3. 유휴 회수의 되풀이에 간격 늘리기(되풀이할 때마다 두 배, 앱을 열면 처음으로)를 둔다. 다시 찬 캐시가 대부분 내려놓은 작업 집합이라면 CPU 낭비가 줄고, 그 대가로 다시 찬 메모리가 게스트에 더 오래 머문다. NFR-3(대기 CPU)을 재야 정할 수 있다.
4. Google 앱과 번역기 이미지를 loop 없이 erofs 파일 마운트에 `directio`로 붙인다(`CONFIG_EROFS_FS_BACKED_BY_FILE`). 지금은 loop가 이미지 파일의 캐시와 erofs의 캐시를 겹쳐 두므로 읽은 만큼이 두 번 잡힌다. 겹친 쪽은 차가워서 다듬기가 먼저 거두지만, 부팅 직후의 캐시 몇백 MiB 가운데 일부가 이것이다. 커널 설정과 init 스크립트를 바꾸고 실기에서 Google Play가 그대로 붙는지 확인해야 한다.

소유자는 4를 구현하고, 3은 정책을 바꾸기 전에 먼저 재도록 정했다(2026-11-10).

### 선택지 4: 이미지를 loop 없이 파일에서 바로 붙인다

**확인한 것.** 고정한 커널은 `linux-msft-wsl-6.18.54.1`(6.18.54)이다. 그 원본을 읽었다(GitHub의 `microsoft/WSL2-Linux-Kernel` 태그, 빌드하지 않았다). 파일 기반 erofs(`CONFIG_EROFS_FS_BACKED_BY_FILE`, 6.12부터)가 있고 기본값이 y다. `fs/erofs/super.c`는 블록 장치가 아닌 원본을 `filp_open`으로 열어 그 파일에서 바로 읽고, `directio` 마운트 옵션(6.12부터)을 받아 `/proc/self/mountinfo`에 `,directio`로 보인다. `fs/erofs/fileio.c`는 읽을 때마다 파일을 연 쪽의 자격(`override_creds(file->f_cred)`, 여기서는 init)으로 `vfs_iocb_iter_read`를 부르고, `directio`이고 아래 파일이 `FMODE_CAN_ODIRECT`일 때만 `IOCB_DIRECT`를 붙인다. 쌓인 파일 시스템 위의 파일이나 파일 기반 erofs 위의 파일은 거절하지만, 블록 장치 위 erofs(`/system`)의 파일은 받는다. 스택 깊이는 늘리지 않으므로 그 위에 overlay를 놓아도 깊이 한도(2)에 닿지 않는다.

**바꾼 것.** 커널 설정에 `CONFIG_EROFS_FS_BACKED_BY_FILE=y`를 적었다(loop 드라이버는 apexd가 쓰므로 남긴다). `dsa-gapps.rc`와 `dsa-arm-translation.rc`의 세 마운트는 `loop@` 없이 이미지 파일을 원본으로 하고, 커널에 넘기는 마지막 인자를 `directio,fscontext=...,context=...`로 했다. SELinux 정책에서는 쓰지 않게 된 init의 loop 장치 규칙을 뺐다. init이 이미지 파일을 읽는 규칙(`vendor_data_file`, `system_file`)과 슈퍼블록 형식, 파일 라벨, bind와 overlay는 그대로다. 시험: `test_android_config.py`의 `test_nfr2_fr25_fr32_the_images_are_mounted_from_their_files_without_a_loop_device`(설정, 고정 커널이 6.12 이상인지, 스크립트에 `loop@`가 없고 `directio`가 있는지), 이미지 부팅 확인 "Lay the ARM translator and Google Play over the booted system"은 두 부팅 모두에서 각 이미지의 마운트가 loop 장치(주 번호 7, `/dev/block/loop*`)가 아니고 원본이 이미지 파일이며 `directio`인지 본다(`overlays.rs`의 `file_backed_problems`).

**기대하는 절감.** loop일 때는 이미지에서 읽은 쪽마다 erofs의 캐시와 이미지 파일(ext4)의 캐시에 한 벌씩 있었고, erofs의 메타데이터는 loop 장치의 블록 캐시에 한 벌 더 있었다. 파일 기반이면 메타데이터는 이미지 파일의 캐시에만 있고(몇 MiB), `directio`가 실제로 걸리면 데이터의 두 번째 벌이 없어진다. 부팅 직후 Google 앱이 더하는 캐시 290-650 MiB 가운데 erofs 쪽과 같은 크기의 두 번째 벌, 많게는 그 절반(약 150-300 MiB, 추정)이다. 다만 `directio`가 걸리는지는 아래 파일 시스템이 정한다.

- Google 앱 이미지(`/data/vendor/dsa/gapps/system.img`)는 `/data`의 ext4 위에 있고, `/data`는 파일 단위 암호화(`fileencryption=aes-256-xts:aes-256-hctr2`)에 `inlinecrypt`가 없다. 6.18의 ext4는 인라인 암호화가 아닌 암호화 파일에 직접 I/O를 주지 않고(`ext4_dio_alignment`, `fscrypt_dio_supported`) 보통의 캐시 읽기로 돌린다. 그래서 지금의 이미지에서는 Google 앱의 두 번째 벌이 그대로 남고, 줄어드는 것은 메타데이터와 loop 장치뿐이다. 실패는 아니다. 읽기는 예전처럼 캐시를 거친다.
- 사용자가 설치한 번역기(`/data/vendor/dsa/arm-translation/system.img`)도 같다.
- 기본 번역기(`/system/etc/dsa/arm-translation/default.img`)는 `/system`의 erofs 위에 있다. erofs는 압축하지 않은 파일에만 직접 I/O를 준다. 이미지의 `/system`이 그 파일을 압축해 담았는지는 확인하지 못했다. 압축하지 않았다면 ARM 앱이 도는 동안 번역기의 두 번째 벌이 없어진다.

Google 앱에서 절감을 실제로 얻는 길은 둘이다. 소유자가 정할 일이라 하지 않았다. (가) `/data`를 `inlinecrypt`로 붙이고 커널에 `CONFIG_BLK_INLINE_ENCRYPTION`, `CONFIG_BLK_INLINE_ENCRYPTION_FALLBACK`, `CONFIG_FS_ENCRYPTION_INLINE_CRYPT`를 넣는다. 정책 플래그가 같으면 디스크 형식은 같지만 `/data`의 모든 읽기와 쓰기가 블록 계층의 소프트웨어 암호화로 바뀐다. 쓰기마다 바운스 페이지가 들고 성능은 재 봐야 한다. (나) Google 앱 이미지를 암호화하지 않는 곳(호스트가 붙이는 읽기 전용 디스크 등)에 둔다. 서비스, dsa-channeld, 이미지의 위치가 함께 바뀐다.

### 선택지 3을 정하기 위한 측정

이미지 부팅 확인에 "Measure the idle reclaim with Google Play on the booted image"를 더했다(`overlays.rs`의 `nfr3_the_idle_reclaim_with_google_apps_is_measured_on_the_booted_image`, `tests/idle_reclaim`). 측정이므로 잴 수 없을 때만 실패한다.

- 조건: 게스트 4096 MiB, 프로세서 2개, 네트워크 없음, 앱 창 없음. Google 앱은 LiteGapps가 게스트의 API 수준에 내는 최신 Core 빌드를 확인이 미러로 내려받아 설치한다(사용자가 받는 빌드). LiteGapps에 닿지 못하면 확인용의 작은 빌드로 재고 경고를 남긴다. 그 빌드에는 도는 Play 서비스가 없으므로 그때의 숫자는 이미지를 붙인 시스템의 숫자일 뿐이다. 네트워크가 없으므로 Play 스토어의 업데이트와 동기화는 일어나지 않는다. 소유자 PC의 값과는 그만큼 다르다.
- 방법: Android가 Google 앱과 함께 다시 시작한 뒤 300초 동안 게스트 안의 셸 스크립트가 1초마다 본다. 셸 내장 명령으로만 읽으므로 한 번 보는 데 `sleep` 말고는 프로세스를 만들지 않는다. 보는 것은 `/proc/stat`, init과 그 subcontext(벤더 스크립트의 `write`를 실행한다)와 kswapd0, kcompactd0의 `/proc/<pid>/stat` 시간, `/proc/vmstat`의 `workingset_refault_file`, `pgpgin`, `pswpin`, `pswpout`, `pgmajfault`, `compact_stall`, `/proc/meminfo`의 `Cached` 등이다. 회수의 커널 일은 쓰는 프로세스 안에서 일어나므로, init이 한 번 볼 때 30 ms 이상 커널 시간을 쓴 구간을 회수 한 번으로 센다. 종류는 게스트 로그(`logcat -v monotonic`)의 dsa-channeld 줄로, 없으면 zram으로 내보낸 페이지가 있었는지로 가른다. 단계별 시간(zram, `drop_caches`, 압축, 다듬기)은 init이 50 ms 이상 걸린 명령에 남기는 줄에서 읽는다.
- 무엇이 캐시를 다시 채우는지: 10초마다 모든 프로세스의 major fault(매핑으로 읽어 들인 파일 페이지: APK, dex, 라이브러리)와 각 최상위 cgroup의 `workingset_refault_file`을 읽어, 회수 사이의 틈마다 늘어난 만큼을 프로세스별로 모은다. user 빌드에서 셸은 자기 프로세스의 `/proc/<pid>/io`만 읽을 수 있어서 `read_bytes`는 대부분 비어 있다. `read()`로 읽는 것은 major fault에 잡히지 않는다.
- 결과: 작업 요약과 아티팩트 `idle-reclaim-measurement`(`idle-reclaim.md`, `idle-reclaim.json`, 원자료 `idle-reclaim-samples.txt`, `idle-reclaim-log.txt`). 회수마다 시각, 종류, 앞 회수와의 간격, init의 커널 시간, 단계별 시간, 앞 회수 이후의 refault와 `pgpgin`, 전후의 캐시, zram으로 간 양을 적고, 전체로는 횟수, 평균 간격, 커널 시간 합, kswapd와 kcompactd의 시간, 게스트 CPU 합(한 프로세서의 몇 %인지)과 그 가운데 측정 스크립트 몫을 적는다.
- 해석할 때: `pgpgin`은 블록 장치에서 읽은 양이다. loop일 때는 같은 읽기가 loop 장치와 `/data`에서 두 번 세였지만, 파일 기반 erofs는 bio를 내지 않으므로 이제 한 번만 센다. v0.2.1 로그와 견줄 때 그만큼 줄어 보인다. 아직 CI의 값은 없다. 첫 develop 실행이 남긴다.

### Play 서비스의 데스크톱에 쓸모없는 상주 구성 요소를 끈다 (FR-32)

**조사.** 소유자 PC의 게스트(Android 17, API 37, Play 서비스 26.43.30, 업데이트된 시스템 앱, 부팅 뒤 71분, 창 없음)에서 읽기만 했다: `dumpsys activity services com.google.android.gms`, `dumpsys package com.google.android.gms`, `dumpsys notification`, `dumpsys location`, `dumpsys meminfo -s com.google.android.gms`, `ps -A`. 상태를 바꾸는 명령은 쓰지 않았다.

- Play 서비스 프로세스는 `com.google.android.gms`와 `com.google.android.gms.unstable` 둘이다. 예전의 `.persistent` 프로세스는 이 버전에서 주 프로세스에 합쳐져 따로 없다. 주 프로세스: RSS 36 MiB, TOTAL PSS 81 MiB 가운데 zram에 나간 몫 71 MiB, AppContexts 387개(Chimera 모듈마다 컨텍스트가 하나씩 있다).
- 돌고 있는 서비스는 31개다. 그 가운데 시스템이 묶은 것(위치 제공자, 지오코더, GNSS 보조, 활동 인식 제공자, Trust의 장소 제공자)과 Play 서비스가 부팅 때 스스로 시작해 계속 두는 것(`startRequested=true`)이 있다. 여러 API는 공용 중개 서비스(`chimera.PersistentApiService`, `PersistentApiServiceNoInstantApps`, `PersistentInternalBoundBrokerService`) 하나에 `chimera-action:` 바인딩으로 들어 있다. 이것들은 구성 요소 하나로 끌 수 없고, 끄면 푸시와 계정까지 멈춘다.
- Play 서비스는 자기 구성 요소를 스스로 켜고 끈다. `enabledComponents`에 `thunderbird.EmergencyPersistentService`, `motionsicknessassist.service.MotionSicknessAssistService` 같은 이름이 명시적으로 켜져 있고, `disabledComponents`에도 Play 서비스가 직접 끈 것(`droidguard.DroidGuardPersistentService` 등)이 있다. 모듈이 내려받히거나 서버 플래그(Phenotype)가 바뀌면 이 상태를 다시 쓴다. 그래서 한 번 끄는 것으로는 남지 않는다.

**끄는 것.** 모두 Play 서비스의 주 프로세스에서 도는 서비스다. 프로세스를 끝내지 않고(`DONT_KILL_APP`) 끄면 활동 관리자가 그 서비스만 내린다.

| 구성 요소 | 하는 일 | 데스크톱에서 끄는 이유 |
|---|---|---|
| `crisisalerts.init.CrisisAlertsPersistentService` | 위치 기반 재난 알림 | 들고 다니는 휴대폰을 위한 기능이다. PC는 한곳에 있고, 재난 알림은 사용자의 휴대폰이 받는다 |
| `thunderbird.EmergencyPersistentService`, `thunderbird.EmergencyLocationService` | 긴급 전화·문자 때 위치 전송(ELS) | 게스트에 전화와 문자가 없다 |
| `motionsicknessassist.persistent.MotionSicknessAssistPersistentService`, `.service.MotionSicknessAssistService`, `.service.MotionAssistSysUiService`, `.settings.MotionSicknessAssistTileService` | 차 안에서의 멀미 방지 표시 | 차 안의 휴대폰을 위한 기능이다. 표시할 SystemUI(빠른 설정 타일)도 없다 |
| `nearby.presence.service.LifeCycleService` | 근처 기기 존재 알림(BLE) | 게스트에 블루투스가 없다(`pm list features`에 없다). 앱이 쓰는 Nearby Connections는 공용 중개 서비스에 있어 그대로다 |
| `cast.devicesuggestions.SuggestionsProviderChimeraService` | 출력 전환기의 Cast 기기 추천 | SystemUI의 출력 전환기가 없다. 앱의 Cast API는 그대로다 |
| `remotegestures.api.RemoteGesturesService` | 시계의 제스처로 폰 조작 | 짝지은 시계가 없다 |
| `com.google.android.location.internal.server.HardwareArProviderService` | 센서 허브의 하드웨어 활동 인식을 플랫폼에 잇는 제공자(`ActivityRecognitionProvider`) | 게스트에 하드웨어 활동 인식이 없다. 앱의 활동 인식 API와 위치 제공자는 다른 서비스라 그대로다 |
| `mdm.services.SecureLockService` | 내 기기 찾기의 원격 "기기 잠금" 화면 | 게스트에 잠금 화면(keyguard)이 없고, PC 잠금은 호스트가 한다. 위치 찾기, 소리 내기, 초기화는 다른 서비스(`DeviceManagerApiService`, `RingService`, 기기 관리자 수신기)라 그대로다. 대가: 내 기기 찾기의 원격 잠금이 이 VM을 잠그지 않는다 |
| `dtdi.lifecycle.LifecycleService` | 기기 간 연동(Phone Hub, 다른 기기로 앱 넘기기) | 이 게스트가 데스크톱 자체다. 부팅 완료 방송으로 시작해 계속 남는다 |

**남겨 두는 것과 그 이유.** `gcm.GcmService`와 `gcm.nts.SchedulerService`(푸시, FCM), 공용 중개 서비스 셋(Phenotype, 계정 이벤트, 텔레메트리 등이 함께 들어 있다), 위치 서비스(`FusedLocationService`, `NetworkLocationService`, `GoogleLocationManagerService`, `GoogleLocationService`, `GeocodeService`, `GnssAssistanceService`, `LocationPersistentService`: 앱의 위치), `play.integrity.api.service.IntegrityApiPersistentService`와 `droidguard.DroidGuardService`(Play Integrity), `fido.authenticator.service.AuthenticatorService`(패스키), `auth.setup.devicesignals.LockScreenService`(계정 설정의 기기 신호), `advancedprotection.service.AdvancedProtectionSyncService`(계정 보호), `contextmanager.service.ContextManagerService`(앱이 쓰는 Awareness API), `auth.proximity.firstparty.SystemMemoryCacheService`(CryptAuth 기기 등록과 이어져 있어 계정에 영향이 없다고 확인하지 못했다), `common.stats.GmsCoreStatsService`, `clearcut.debug.ClearcutDebugDumpService`. 백업 전송 서비스는 돌고 있는 목록에 없었다. 시스템의 백업 관리자가 백업할 때 묶으므로 그대로 둔다. Phone Hub의 알림 수신기(`PhoneHubNotificationListenerService`)는 켜져 있지만 알림 수신기로 허용되지 않아 묶여 있지 않으므로 끄지 않았다.

**다시 켜지는 길과 다시 끄는 방법.** 에이전트(`DesktopPlayServices`)가 시작할 때(부팅마다), 그리고 `com.google.android.gms`의 `PACKAGE_ADDED`, `PACKAGE_REPLACED`, `PACKAGE_CHANGED`를 받고 2초 뒤에 상태를 읽어 켜진 것을 한 번의 `setComponentEnabledSettings`로 끄고 로그에 남긴다("turned off Google Play services components a desktop does not use: ..."). Play 서비스가 모듈을 받거나 플래그로 구성 요소를 켜는 일도 패키지 관리자를 거치므로 모두 `PACKAGE_CHANGED`로 온다. 그래서 따로 Phenotype을 막을 필요가 없다. Phenotype 플래그를 밖에서 바꾸는 길은 user 빌드에 없다(플래그 저장소는 Play 서비스의 비공개 데이터이고, 플래그 덮어쓰기 방송은 디버그 빌드에서만 받는다). 시스템 설정의 `component-override`는 매니페스트의 기본값만 바꾸므로 Play 서비스가 명시적으로 켠 구성 요소에는 효과가 없어 쓰지 않았다. Play 서비스가 같은 구성 요소를 한 시간에 다섯 번 넘게 다시 켜면 에이전트는 그 구성 요소를 그 부팅 동안 두고 경고를 남긴다. 끝없이 다투면 CPU를 쓰기 때문이다. Play 서비스의 버전이 구성 요소를 없애거나 이름을 바꾸면 그 이름은 건너뛴다(패키지 관리자는 없는 구성 요소가 섞인 묶음 전체를 거절한다).

**시험.** 에이전트: `gapps/DesktopPlayServicesTest.kt`(`fr32_desktop_irrelevant_play_services_stay_off_after_updates`, 가짜 패키지 관리자). 이미지 부팅 확인: CI의 작은 Google 앱 빌드는 서명되지 않아 Play 서비스가 설치되지 않으므로, Play 서비스 이름으로 이 서비스들과 `GcmService`를 선언한 코드 없는 앱(`dsa/guest/gms-probe`)을 두 번째 부팅 뒤 설치한다. 끌 것이 꺼지고 `GcmService`가 켜져 있는지, 셸에서 다시 켜고 자기 위에 다시 설치(업데이트 방송)한 뒤 다시 꺼지는지 본다(`overlays.rs`의 `play_services_problems`).

**절감.** 재지 않았다. 이 조사는 게스트의 상태를 바꾸지 않았고, 끄는 것은 프로세스를 없애지 않는다. 그래서 기대하는 몫은 Play 서비스 주 프로세스 안의 그 서비스들의 모듈 코드와 힙, 그리고 그것들이 부팅 뒤 계속 하는 일(재난 알림과 ELS의 위치 요청, 근처 기기 탐색, 기기 간 연동의 상태 유지)이다. 어림으로 주 프로세스 PSS의 수 MiB에서 20 MiB 안쪽과, 깨어남과 부팅 직후 CPU가 줄어든다. 위의 "선택지 3을 정하기 위한 측정"은 실제 LiteGapps 빌드를 쓰므로 이 변경 뒤로는 이 서비스들을 끈 상태로 잰다. 확인: 소유자 PC에서 이 변경 앞뒤로 같은 조건(Google Play 설치, 창 없음, 부팅 10분 뒤)에서 `dumpsys meminfo -s com.google.android.gms`의 TOTAL PSS와 TOTAL SWAP PSS, `dumpsys activity services com.google.android.gms`의 서비스 수, `dsa memory`의 앱 메모리를 남긴다.

### 확인하지 못한 것

- 0007의 효과 전부. 이 PC에서 빌드하거나 실행하지 않았다. Graphics test와 이미지의 그래픽 부팅 확인(Vulkan CTS 일부)이 먼저 판단하고, PC에서는 같은 조건(Google Play, YouTube 창 하나)으로 `dsa memory`의 앱 메모리와 PSS, `dsa-wm.log`의 렌더러 줄, 그리고 `androidboot.hardware.dsa_input_trace=1`일 때의 `stage=driver.flush` 줄(`scanned_mb_per_submit`)을 앞뒤로 잰다.
- 프레임마다의 다시 읽기가 실제로 몇 MiB였는지. 0005 뒤의 읽어 오기에는 추적 줄이 없다.
- 창이 없을 때 캐시를 다시 채우는 것이 무엇인지. 게스트에서 `/proc/vmstat`의 `workingset_refault_file`과 `pgpgin`을 회수 전후로 함께 남기면 가를 수 있다. 위의 측정이 그것을 남긴다. 그 값은 아직 없다.
- 파일 기반 erofs로 바꾼 뒤 실제로 줄어든 캐시. CI는 SELinux 강제 모드에서 마운트가 되는지와 마운트의 모양만 본다. 소유자 PC에서 Google Play를 둔 채 부팅 직후의 `dsa memory` 캐시 값을 v0.2.1과 견줘야 한다. 기본 번역기가 `/system` 안에 압축되어 있는지도 확인하지 못했다.
- VM을 줄이는 HCS 요청이 시간 초과로 실패한 원인.

### Google Play의 부팅 뒤 되풀이와 앱 이미지 (2026-11-10)

**기준.** 소유자 PC에서 실행 중인 게스트(Android 17 CP2A.260605.016, Play 서비스 26.43.30, Play 스토어 54.0.09)를 adb로 읽기만 했다(`logcat -d`, `dumpsys`, `/proc`, `getprop`). 부팅 65-76분 뒤에 읽었다. Google Play는 그 부팅 18분 전에 설치됐고, 부팅 4분 전에 Play 스토어가 Play 서비스와 자신을 업데이트했다. main 로그 버퍼(256 KiB)에는 부팅 20분 뒤부터만 남아 있어서, 첫 10분은 events와 system 버퍼, 호스트의 사용자별 안드로이드 로그(커널과 init)로 읽었다. ART Service와 프레임워크의 동작은 AOSP main 소스(art `libartservice`, `artd`, frameworks/base `PackageManagerService`, `DexOptHelper`)로 확인했다.

**찾은 것.**

1. RETRY_UPDATE 폭주의 원인은 장치 구성자(device configurator)가 없는 것이다. `dumpsys package`가 "Configurator: none"을 보인다. WRITE_DEVICE_CONFIG, WRITE_ALLOWLISTED_DEVICE_CONFIG, READ_WRITE_SYNC_DISABLED_MODE_CONFIG의 보호 수준은 `signature|verifier|configurator`이고, 구성자는 `config_deviceConfiguratorPackageName`이 정한다. AOSP 기본값은 빈 문자열이고 Google 기기는 `com.google.android.gms`로 둔다. 그래서 Play 서비스의 설치 권한에 WRITE_DEVICE_CONFIG가 없다(Play 스토어는 검증자라서 있다). Phenotype이 받은 플래그를 DeviceConfig에 쓰는 platformconfigurator는 쓰기를 거절당하고 다시 시도한다. `dumpsys alarm`: `*alarm*:com.google.android.gms.platformconfigurator.RETRY_UPDATE` 720번, 실행 시간 합 2.7초. POST_BOOT(부팅 약 2분 뒤)부터 약 10분 동안(마지막은 부팅 약 12분 뒤) 1초에 한 번 남짓이다. Play 서비스의 다른 알람은 8번 이하다. DeviceConfig 자체는 돈다(`cmd device_config list` 3119개, 동기화 끔 없음). 휴대전화에는 없는, DSA 이미지 쪽의 차이다.
2. 함께 본 다른 되풀이.
   - system_server의 `AiSealSystemService`가 `aiseal_internal` 서비스를 1초마다 찾는다(약 1시간에 3599번). 그 서비스를 내는 `aisealhostservice`는 `service.aiseal.enable=1`일 때에만 시작하고, 이미지는 이 속성을 두지 않는다. 되풀이는 부팅 8분 뒤 shell(uid 2000)의 요청과 같은 때 시작했으므로, 조사 중의 `dumpsys`가 첫 연결을 부른 것으로 보인다. Google Play와는 관계가 없다. 고치지 않았다.
   - Play 서비스의 Cast 탐색(`AOSP-MdnsDiscoveryManag`)이 멀티캐스트 `sendto`에서 EPERM을 받는다(76번).
   - 업데이트된 Play 서비스와 Play 스토어(`/data/app`)의 런타임 권한이 `dumpsys package`의 활성 패키지 항목에서 모두 granted=false이고, 숨은 시스템 패키지 항목에만 GRANTED_BY_DEFAULT가 있다. 출력 방식의 차이인지 실제로 기본 권한을 잃은 것인지는 가르지 못했다.
3. 유휴 중 캐시 재충전. 창이 없는 Idle에서 60초 동안 `workingset_refault_file` +37.5k 페이지(146 MiB), `pgpgin` +133 MiB, `pgsteal_file` +116k 페이지(455 MiB)이고, Cached는 625에서 342 MiB로 줄었다. 부팅부터 76분 동안 refault 2.27M 페이지(8.7 GiB), `pgpgin` 37 GiB다. `pgsteal_kswapd`와 `pgsteal_direct`가 0이므로 페이지 캐시를 거두는 것은 모두 dsa-channeld의 회수(10-15초마다 "reclaiming the guest's memory (Idle, 0 tasks in use)")다. 같은 동안(약 2.5분) 프로세스별 major fault(`/proc/<pid>/stat`): YouTube(캐시 프로세스) 7794, Play 서비스 7372, Play 스토어 4399, system_server 1075, ext.services 954. 다시 찬 캐시는 대부분 이 프로세스들이 다시 읽은 코드와 데이터다. RETRY_UPDATE가 멈춘 뒤에도 이어지므로 폭주만의 문제가 아니라 회수 정책 쪽(위 선택지 3)의 문제다. 그 정책은 다른 작업이 맡고 있어 여기서 바꾸지 않았다.
4. 앱 이미지. Play 서비스 base.apk는 `[status=speed-profile] [reason=install-dm]`(Play 스토어 업데이트의 클라우드 프로필)이다. `dumpsys meminfo -d`의 `.App art`: com.google.android.gms Pss 2.9 MiB와 SwapPss 27.8 MiB. com.google.android.gms.unstable은 `.art mmap`(앱 이미지와 부트 이미지의 합) SwapPss 29.5 MiB다. 앱 이미지는 프로세스마다 풀어서 자기 메모리에 올리므로 프로세스 사이에 공유되지 않는다.

**Android 16/17에서 패키지 하나의 앱 이미지를 없애는 방법.**

- ART Service(`Dexopter.getDexoptOptions`): `generateAppImage = isProfileGuidedFilter && !dalvik.vm.appimageformat.isEmpty()`. 이 속성은 컴파일할 때마다 읽는다. artd는 앱 이미지를 만들지 않는 컴파일에서 있던 `.art`를 지운다(`files_to_delete`). `getDexoptNeeded`는 앱 이미지를 보지 않으므로, 앱 이미지 없이 같은 필터로 컴파일된 패키지를 백그라운드 작업이 이 까닭으로 다시 컴파일하지는 않는다(프로필이 충분히 늘면 다시 컴파일하고, 그때 앱 이미지가 다시 생긴다).
- 패키지별 필터 속성은 SystemUI용(`dalvik.vm.systemuicompilerfilter`)뿐이고, 런처는 speed-profile로 고정된다. 그 밖의 패키지별 조정(`ArtManagerLocal.setAdjustCompilerFilterCallback`, `setBatchDexoptStartCallback`)은 system_server 안에서만 쓸 수 있어 services.jar를 고쳐야 한다.
- `pm compile`과 `pm art`는 root와 shell만 부를 수 있고(`ArtShellCommand.enforceRootOrShell`), `IPackageManager.performDexOptMode`는 system, root, shell과 그 패키지의 설치자만 부를 수 있다. 에이전트(uid 10068)는 어느 것도 아니다.
- 플랫폼 정책에 이미 `domain_trans(init, shell_exec, shell)`이 있어서(console 서비스용) init은 정책을 더하지 않고 `seclabel u:r:shell:s0` 서비스를 띄울 수 있다.
- verify로 컴파일하면 앱 이미지는 없지만 AOT 코드도 없다. 코드가 JIT로만 돌면 JIT 코드 캐시가 프로세스마다 생기고(공유되지 않는다) CPU가 더 들며, 백그라운드 작업이 다시 speed-profile로 올린다. 그래서 같은 프로필의 speed-profile을 앱 이미지 없이 다시 만드는 쪽을 골랐다.

**고친 것.**

1. 프레임워크 오버레이(`DsaFrameworkOverlay`)에 `config_deviceConfiguratorPackageName=com.google.android.gms`. Google Play가 없으면 그 패키지가 없으므로 구성자는 전처럼 없다. Google Play가 있으면 다음 부팅의 권한 갱신에서 Play 서비스가 WRITE_DEVICE_CONFIG를 받는다. 시험: `test_nfr2_google_play_services_is_the_device_configurator`, overlays 부팅 확인의 `cmd overlay lookup`.
2. `gapps-compile.sh`와 `dsa-gapps.rc`. Google Play가 켜진 부팅에서 sys.boot_completed 때와, 에이전트가 `vendor.dsa.gapps.compile`을 바꿀 때마다(Play 서비스나 Play 스토어가 설치되거나 업데이트될 때, 화면이 꺼질 때) init이 `dalvik.vm.appimageformat`을 비우고 스크립트를 shell로 돌린다. 스크립트는 base.apk가 profile-guided이고 reason이 cmdline이 아닌 패키지만 `cmd package compile -m speed-profile -p PRIORITY_BACKGROUND -f`로 다시 컴파일한다. 스크립트가 끝나면 init이 형식을 빌드 값으로 되돌린다(post-fs-data에 `vendor.dsa.gapps.appimageformat`에 둔 값, 없으면 lz4). 시험: `GoogleAppsCompileTests`(rc, 속성, 정책, 가짜 `cmd`로 돌린 스크립트), 에이전트의 `GoogleAppsCompileTest`.
3. 부팅 확인. overlays 확인의 Google 앱 부팅에서 구성자 값, 컴파일 서비스가 끝까지 돌고 형식을 lz4로 되돌렸는지, 알람 폭주(부팅부터 60번 이상이고 평균 10초에 한 번보다 잦음, `nfr2_an_alarm_retried_every_second_after_boot_is_a_storm`)를 본다. CI의 Google 앱은 서명 없는 작은 빌드라 실제 Play 서비스가 설치되지 않는다. 그래서 CI는 오버레이 값, 스크립트, 모든 알람의 폭주만 보고, Play 서비스의 RETRY_UPDATE와 다시 컴파일은 실기에서 본다.

**기대하는 절감과 비용 (재지 않은 추정).**

- 앱 이미지: 늘 떠 있는 Play 서비스 두 프로세스에서 `.App art` 약 25-30 MiB씩이 빠진다. 앱 이미지가 없으면 각 프로세스가 쓰는 클래스를 실행하면서 Dalvik 힙에 올리므로 순 절감은 그보다 작다. 클래스를 조금 쓰는 gms.unstable(DroidGuard)은 대부분, 메인 프로세스는 일부로 보아 합쳐 약 25-45 MiB(PSS, zram에 있는 몫 포함)를 기대한다. Play 스토어의 프로세스들(`:background`, `:quick_launch` 등)에서도 각자의 몫이 빠진다(크기는 재지 않았다).
- 비용: Play 서비스나 Play 스토어가 업데이트되면 한 번 다시 dex2oat한다(백그라운드 우선순위, 수십 초에서 몇 분의 CPU). 화면이 꺼질 때마다 `cmd package art dump` 두 번. Play 서비스 프로세스의 시작은 클래스를 적재하는 만큼 느려진다(프로세스마다 수십에서 수백 ms로 추정). 스크립트가 도는 동안 컴파일되는 다른 앱도 앱 이미지 없이 컴파일된다. 이미 떠 있는 프로세스는 다시 시작할 때까지 예전 앱 이미지를 쥐므로 절감은 그 뒤(대개 다음 부팅)부터다.
- 폭주: 부팅 뒤 약 10분 동안의 RETRY_UPDATE 약 720번이 없어지고, 그만큼 Play 서비스 프로세스가 깨지 않는다.

**확인하지 못한 것.**

- 실기의 다음 부팅에서 `dumpsys package`의 "Configurator: com.google.android.gms", Play 서비스의 WRITE_DEVICE_CONFIG granted=true, `dumpsys alarm`의 RETRY_UPDATE 횟수.
- 스크립트가 실제 Play 서비스를 다시 컴파일하는지(`logcat -s DsaGappsCompile`, `pm art dump com.google.android.gms`의 reason=cmdline), 그 시간, 그 뒤 `dumpsys meminfo -d`의 `.App art`. toybox sed가 스크립트의 sed 식을 GNU sed와 같게 다루는지(단위 시험은 GNU sed로 돈다).
- 절감과 시작 시간 비용의 숫자.
- 유휴 refault(위 3)의 정책 쪽 처리, AiSeal의 1초 되풀이(`service.aiseal.enable`을 둘지, 첫 연결을 막을지), mDNS EPERM, 업데이트된 Google 앱의 기본 권한 상태.
- 구성자가 된 Play 서비스는 이제 DeviceConfig에 실제로 쓴다. 그래서 아래 추가 조사의 `activity_manager/max_cached_processes`처럼 에이전트가 정해 두는 플래그를 Google의 값으로 바꿀 수 있고, 에이전트는 그때마다 다시 쓴다. 두 쪽이 얼마나 자주 번갈아 쓰는지는 실기에서 `dumpsys device_config`와 에이전트 로그로 본다.

### 추가 조사 (2026-11-10): Google Play 서비스가 실제로 드는 메모리와 캐시 프로세스 상한

**기준.** 소유자 PC, Android 17 게스트, Google Play 설치, 열린 창 없음, 부팅 뒤 58분. 읽기만 하는 조사였다(`dumpsys meminfo`, `/proc/meminfo`, zram의 `mm_stat`, `settings get global activity_manager_constants`, `dumpsys activity settings`). 빌드하거나 설치하지 않았다.

**잰 것.**

- Google Play 서비스의 PSS 약 140 MiB 가운데 92%가 이미 zram에 있는 SwapPss였다. `com.google.android.gms`는 PSS 81 MiB 중 SwapPss 74.6 MiB, `com.google.android.gms.unstable`은 54 MiB 중 49 MiB. zram 압축비는 3.14였다. 그래서 호스트가 실제로 내는 몫은 약 50 MiB다.
- 안드로이드의 PSS(`Debug.MemoryInfo.getTotalPss`)는 SwapPss를 압축 전 크기로 더한다. DSA의 보고(`dsa memory`, 로컬 API, 세 설정 앱)가 이 값을 그대로 보여 주었으므로 유휴 게스트가 실제 비용의 약 2.7배로 보였다.
- 깨울 때마다(`dumpsys` 한 번에도) gms의 RSS가 30 MiB에서 73 MiB로 돌아왔고, 다음 전역 유휴 회수(`dsa/guest/channeld/src/reclaim.rs`, cgroup `memory.reclaim`)까지 그대로 머물렀다.
- `settings get global activity_manager_constants`는 `max_cached_processes=8`이었지만 `dumpsys activity settings`는 `max_cached_processes=1024`, `CUR_MAX_CACHED_PROCESSES=1024`였다. R8의 32에서 8로 낮춘 상한이 Android 17에서는 적용되지 않았다.

**캐시 프로세스 상한을 읽는 곳 (AOSP `frameworks/base` main, `ActivityManagerConstants.java`).** 적용되는 값은 `CUR_MAX_CACHED_PROCESSES`다. `updateMaxCachedProcesses()`가 DeviceConfig의 `activity_manager` 네임스페이스 `max_cached_processes`를 읽고, 비어 있으면 프레임워크 리소스 `config_customizedMaxCachedProcesses`(기본 1024)를 쓴다. 전역 설정 `activity_manager_constants`의 파서는 이 키를 더 이상 읽지 않는다. `dumpsys`의 `max_cached_processes=` 줄은 상수 `MAX_CACHED_PROCESSES`(1024)로, 상한과 비교만 될 뿐 바뀌지 않는다.

**바꾼 것.**

1. 보고: 프로세스의 비용은 상주분(PSS에서 SwapPss를 뺀 것)과 zram 안의 압축된 몫(SwapPss에 zram이 잰 비율, `mm_stat`의 사용량 나누기 저장량을 곱한 것, 저장량이 없으면 `/proc/meminfo`의 사용 중인 스왑)의 합이다. 비율을 잴 수 없으면 압축 몫은 "알 수 없음"으로 두고, 압축 전 크기로 세지 않는다. 에이전트는 모든 프로세스의 SwapPss 합과 Google Play 서비스(gms, vending, gsf와 Google 앱 이미지가 들여온 시스템 앱) 전체를 따로 보낸다. `dsa memory`, 로컬 API, 세 설정 앱의 메모리 페이지가 Google Play 서비스를 따로 한 줄로 보여 준다.
2. 깨어난 뒤의 회수: `dsa-channeld`가 2초마다 앱 프로세스를 보고, CPU를 쓴 뒤 6초 조용한 프로세스의 사적 익명 영역을 `process_madvise(MADV_PAGEOUT)`로 zram에 보낸다. 포그라운드 앱과 사용자가 보는 프로세스(`oom_score_adj` 100 미만), 태스크 창이 열린 패키지는 건드리지 않는다. 다른 앱은 perceptible(200)부터, Google Play 서비스는 visible(100)부터 대상이다. 한 번에 한 프로세스, 같은 프로세스는 30초에 한 번, 지난번 뒤로 2 MiB 넘게 늘었을 때만 한다. 줄마다 옮긴 양을 로그에 남긴다. init이 `SYS_NICE`와 `SYS_PTRACE`를 주고, 정책이 그 둘을 허용한다.
3. 캐시 프로세스 상한: 프레임워크 오버레이가 `config_customizedMaxCachedProcesses`를 8로 두고(첫 부팅부터), 에이전트가 DeviceConfig `activity_manager/max_cached_processes`를 8로 쓰며 다른 쪽(Google Play 서비스의 플래그 동기화)이 바꾸면 다시 쓴다. 이전 에이전트가 전역 설정에 쓴 값은 지운다. 오버레이 부팅 확인이 두 부팅 모두에서 `dumpsys activity settings`의 `CUR_MAX_CACHED_PROCESSES=8`을 확인한다.

**확인하지 못한 것.**

- 실기에서의 효과. 같은 조건(Google Play, 창 없음, 깨우기 한 번)으로 `dsa memory`의 Google Play 서비스 줄과 channeld의 `paged out` 로그 줄, gms의 RSS를 앞뒤로 재야 한다.
- 플랫폼 정책에는 `sys_ptrace`를 소수 도메인에만 허용하는 neverallow가 있다. 이미지는 `secilc -N`으로 정책을 컴파일하므로 부팅은 막히지 않지만, 호환성 시험(VTS)에서는 걸릴 수 있다.
- Google Play 서비스가 DeviceConfig `activity_manager`를 실제로 바꾸는지, 바꾼다면 얼마나 자주인지.

### v0.2.2로 올린 뒤 유휴 VM 상주가 늘었다는 보고 (2026-11-11)

**보고.** v0.2.1에서 v0.2.2로 올린 뒤(Windows 11 Pro 26300, x64, 32 GiB, Android 17 이미지 -13, Google Play 설치), 창이 없고 안드로이드가 Doze일 때 서비스 로그의 `memory: host` 줄이 v0.2.1은 host 473-562, vm 432-518(게스트 사용 659-756), v0.2.2는 host 806-967, vm 728-893(게스트 사용 647-802)이었다. 게스트가 쓰는 양은 같은데 VM의 호스트 상주(vmmem의 작업 집합)가 게스트 사용 아래로 내려가지 않는다는 것이었다.

**방법.** 빌드하거나 실행하지 않았고, 바탕 화면과 설치된 DSA는 건드리지 않았다. 읽은 것: `C:\ProgramData\DSA\logs\dsa-hostd.log`(두 판의 줄이 함께 있다), 사용자별 안드로이드 로그(v0.2.1의 마지막 부팅 2026-11-09T19:54:12Z, v0.2.2의 부팅 2026-11-11T00:40:20Z), `v0.2.1..v0.2.2`의 코드 차이, 그리고 실행 중인 v0.2.2에서 `dsa memory`, `Get-Process vmmem*`, 성능 카운터, adb로 `/proc/meminfo`, `/proc/vmstat`, `/proc/diskstats`, `dumpsys deviceidle`.

**찾은 것.**

1. 코드에서 vmmem을 다루는 것은 두 판이 같다. VM 문서의 메모리 설정(`AllowOvercommit`, `EnableDeferredCommit`, `EnableColdDiscardHint`, `EnableColdHint`, 작은 백킹 페이지와 64 KiB 폴트 클러스터, `HostingProcessNameSuffix`)과 커널 명령줄(`page_reporting.page_reporting_order=0`, `swiotlb=2048` 등)은 바뀌지 않았다(커널 명령줄은 두 부팅의 줄을 비교했고, 다른 것은 밀도와 `opengles.version`뿐이다). DSA는 어느 판에서도 vmmem의 작업 집합을 직접 줄이지 않는다(`EmptyWorkingSet`은 창 관리자와 렌더러가 자기 자신에게만 부른다). vmmem이 게스트 사용보다 작아지는 것은 Windows가 호스트 메모리가 모자랄 때 차가운 페이지를 작업 집합에서 내리는 것이고(같은 순간 `vmmemWSL`은 private 1737 MiB 중 182 MiB만 상주했다), 그 동작은 두 판에서 다르지 않다.
2. VM 크기 조절(#562, #564)은 이 비교와 관계가 없다. v0.2.2 로그에는 `Android's memory grows`도 `could not be set`도 없고, 게스트의 MemTotal은 처음부터 2975 MiB 그대로다. 부가 기능 디스크(#578)는 읽기 전용 VHD로 붙을 뿐이고, 이 부팅에서는 Google 앱 이미지를 아직 `/data`에서 붙였다(`sdc1` 읽기 0번). 디스크로 옮긴 이미지는 다음 시작부터 쓰인다. GPU CPU 업로드(#591)는 hvsocket으로 복사하므로 게스트 메모리를 호스트에 고정하지 않는다. 깨어난 뒤의 회수(#572)는 상주 익명 메모리를 줄인다(이 부팅 48번, 한 번에 수 MiB에서 57 MiB).
3. 보고한 v0.2.2의 두 값은 첫 부팅 뒤 5-11분, v0.2.1의 값은 같은 VM이 약 29시간(잠자기 여섯 번 포함) 지난 뒤의 것이다. 첫 부팅에는 이미지 갱신, 앱 이미지 없는 GMS 재컴파일, 부가 기능 디스크 준비가 겹쳤다. 같은 단계끼리 견주면 차이가 없다. v0.2.1의 마지막 부팅에서 YouTube 창을 닫은 뒤 처음 일곱 줄(부팅 뒤 약 27-60분)은 vm 508-932, 게스트 607-975, vm에서 게스트 사용을 뺀 값이 -124에서 +35, 평균 약 -45 MiB였다.
4. 로그 한 줄의 vm과 게스트 수치는 같은 순간의 것이 아니다. v0.2.1 줄의 게스트 보고는 64-215초 전 것이 대부분이고(가장 흔한 값 65 s, 212 s), v0.2.2 줄은 1-3초 전 것이다. 게스트 사용은 회수 주기(캐시가 다시 차고 내려놓기를 10-60초마다 되풀이)에 따라 30초 사이에도 100-190 MiB 오르내린다(아래 표). 그래서 v0.2.1의 "vm이 게스트 사용보다 훨씬 작다"는 일부가 오래된 게스트 수치와 지금의 vm을 견준 것이다.
5. 로그의 "working set"과 "trim" 줄은 게스트 쪽 기록이라 서비스 로그가 아니라 사용자별 안드로이드 로그(`android-<SID>.log`)에 있다(`dsa-channeld: trimming N MiB of cold page cache (working set target N MiB)`). 두 판의 서비스 로그 어디에도 그런 줄은 없고, v0.2.2의 안드로이드 로그에는 이 부팅에서만 23번 있다. 부팅 뒤 처음 20분의 게스트 회수 횟수도 비슷하다(v0.2.1 마지막 부팅 compact 79, reclaim 79, trim 56; v0.2.2 compact 56, reclaim 57, trim 23, pageout 48).

**v0.2.2에서 같은 순간에 잰 값 (부팅 뒤 약 32-35분, Play 스토어 창 1개, `dsa memory`는 게스트에 새 보고를 받는다).**

| 시각 (KST) | vm 상주 | vm private | 게스트 사용 | vm - 게스트 |
|---|---|---|---|---|
| 10:12:20 | 651 | 938 | 694 | -43 |
| 10:13:20 | 815 | 946 | 857 | -42 |
| 10:13:50 | 746 | 880 | 787 | -41 |
| 10:14:21 | 625 | 933 | 671 | -46 |
| 10:14:51 | 782 | 920 | 830 | -48 |
| 10:15:21 | 637 | 945 | 680 | -43 |
| 10:15:51 | 655 | 938 | 707 | -52 |

창이 하나 열린 채로도 vm은 같은 순간의 게스트 사용보다 41-52 MiB 작고, private(커밋)보다 130-310 MiB 작다. 곧 Windows는 v0.2.2의 vmmem도 다듬고 있고, 빈 페이지 보고도 돈다(`dsa memory`: "the VM holds 0 MiB beyond what Android uses"). 부팅 뒤 5-11분의 두 줄(vm - 게스트 +91, +81)은 게스트가 아직 부팅의 일을 하고 Windows가 아직 vmmem을 다듬기 전의 값이다. 이 PC의 호스트 가용 메모리는 그동안 2.4-3.9 GiB였다.

**결론.** v0.2.1과 v0.2.2 사이에 호스트가 VM 메모리를 돌려받거나 다듬는 것을 막은 변경은 찾지 못했다. 보고된 차이는 부팅 뒤 경과 시간이 다른 값을 견준 것과, v0.2.1 줄의 게스트 수치가 몇 분 묵은 것에서 온다. 그래서 고칠 코드를 정하지 않았고 코드는 바꾸지 않았다. 고칠 회귀가 없는데 `nfr2_` 시험을 지어 붙이면 아무것도 지키지 않는다.

**확인하지 못한 것.**

- 창을 모두 닫은 v0.2.2를 몇 시간 둔 뒤의 값. v0.2.1의 432-518 MiB와 견줄 수 있는 것은 그 값뿐이다. 창을 닫는 일은 바탕 화면을 건드리는 것이라 이 조사에서 하지 않았다. 같은 조건(창 없음, Doze, 부팅 뒤 수 시간)에서 `dsa memory`를 30초 간격으로 몇 번 읽어 위 표와 같은 꼴로 남겨야 한다.
- 부가 기능 디스크에서 Google 앱 이미지를 붙이는 다음 부팅의 값. 그때는 `directio`가 실제로 걸려 이미지의 두 번째 캐시가 없어질 것으로 기대한다(재지 않았다).
- v0.2.1 줄의 게스트 보고가 왜 1-3분 묵었는지. 에이전트의 5분 주기와 호스트의 요청 경로는 두 판이 같다.

### v0.2.2에서 창 하나를 보이면 약 390 MiB가 느는 까닭 (2026-11-11)

**보고.** v0.2.2(Windows 11, 노트북 2880x1800 250%와 125% 외부 모니터, 렌더러는 Intel Arc 내장 GPU, Android 17, Google Play 설치)에서 Play 스토어 창 하나만 열고, 최소화하면 작업 관리자의 DSA가 약 600 MB, 복원하면 약 1.3 GB였다. 같은 분의 `dsa memory`: 최소화 host 873 MiB(vm 812, 렌더러 38 상주/563 private, 앱 창 6, 창 관리자 17), 게스트 페이지 캐시 359, 익명 154, 커널 410(zram 133). 보일 때 host 1265 MiB(vm 990, 렌더러 224, 앱 창 33), 페이지 캐시 507, zram 157. 창 크기는 963x1501 px이다.

**방법.** 빌드하거나 실행하지 않았고 바탕 화면과 설치된 DSA는 건드리지 않았다. 읽은 것: `dsa-wm.log`의 렌더러 메모리 줄(같은 부팅, 렌더러 pid 11040과 전날 pid 89524), 사용자별 안드로이드 로그, 읽기만 하는 adb로 `dumpsys SurfaceFlinger`, `dumpsys window windows`, `dumpsys gfxinfo`, `dumpsys meminfo`, `/proc/vmstat`(10초 간격 두 번), `logcat -d`의 `dsa-channeld` 줄, 그리고 고정된 Mesa(`224e91e3`)의 gfxstream 게스트 코드에 이 저장소의 패치를 차례로 적용한 사본.

**찾은 것.**

1. 렌더러의 GPU 할당이 줄지 않고 는다(내장 GPU라 이것이 호스트 RAM이다). pid 11040: 00:44 GPU-only 148 MiB/11개, 00:53 208/13, 237/19, 00:54 291/29, 01:36 403/44, 01:39 475/47. 전날 pid 89524는 1731 MiB/248개까지 갔고, 앱 프로세스가 끝날 때만 줄었다(vending이 끝나자 1091에서 867로). 곧 할당은 게스트 프로세스에 묶여 있고 그 프로세스가 살아 있는 동안 풀리지 않는다. 원인: 게스트 Vulkan 드라이버(`GrallocMinigbm`)는 gralloc 버퍼마다 호스트 ColorBuffer를 한 번 열고 그 참조를 `release(AHardwareBuffer*)`에서만 닫는다. 그 경로는 기기 메모리로 들여온 버퍼(`VkImportAndroidHardwareBufferInfoANDROID`)뿐이다. 창의 버퍼는 Android의 스왑체인이 버퍼마다 `VkNativeBufferANDROID`로 VkImage를 만드는 경로로 오고(hwui는 `Skia (OpenGL)`, GLES는 ANGLE, ANGLE의 창 표면이 그 스왑체인이다), 그 이미지를 없애도 아무것도 닫지 않았다. 그래서 창이 가졌던 버퍼(크기를 바꿀 때마다, 최소화 뒤 복원할 때마다 새로 만든다)가 모두 앱 프로세스가 끝날 때까지 호스트에 남았다.
2. 에이전트의 홈 액티비티(`HomeActivity`, 보이지 않는 홈)가 화면 크기 창을 가졌다. SurfaceFlinger의 버퍼 목록에 `VRI[HomeActivity]` 2880x2880 버퍼 두 개(각 32411 KiB)가 있고, DEVICE 층으로 매 프레임 넘어간다(hwcomposer는 어느 태스크에도 들지 않아 호스트로 보내지 않는다). 게스트 gralloc 63 MiB와 호스트 ColorBuffer 63 MiB가 아무도 보지 않는 투명한 창에 쓰였다.
3. SurfaceFlinger의 클라이언트 타깃(`LegacyFramebufferSurface`) 2880x2880 두 개(63 MiB)가 있다. 화면 크기라 줄일 수 없고, 한 번 클라이언트 합성을 하면 SurfaceFlinger가 끝날 때까지 남는다. 지금은 `usesClientComposition=false`다. 이것은 고치지 않았다(아래).
4. 최소화한 태스크도 hwcomposer가 합성 대상 ColorBuffer 두 개(태스크 버퍼 크기, 이 창은 1024x1536으로 각 6 MiB)를 계속 쥐었다. 창은 자기 스왑체인에 마지막 프레임을 갖고 있으므로 호스트에는 다음 프레임 때만 필요하다.
5. 페이지 캐시: 창이 보이는 동안 작업 집합 다듬기가 10-30초마다 72-147 MiB를 내려놓고(목표 256 MiB), 캐시가 곧 다시 찬다(365, 331, 328, 365, 431, 403, 428 MiB). 10초 동안 `pgpgin` +26 MiB, `workingset_refault_file` +16 MiB, 주 폴트 +63이었다. 곧 다듬기가 가져간 것의 약 60%가 20초 안에 `read()`로 되읽혔다. 한 번 보기(5초)에 되읽힘이 8 MiB라 한계 16 MiB에 못 미쳐 목표가 오르지 않았고, 같은 페이지를 되풀이해 내려놓고 읽었다. 부팅 뒤 약 1시간에 `pgpgin`이 41 GiB, 되읽힘이 6.7 GiB였다. 호스트가 "short"라 60초마다 전체 회수(`drop_caches`)도 돌았다. +148 MiB는 이 톱니의 위쪽을 잰 값이다. 무엇이 1 MiB/s로 새로 읽는지는 shell 권한으로 `/proc/<pid>/io`를 읽을 수 없어 가리지 못했다.
6. 앱 창 프로그램 +27 MiB: 스왑체인 버퍼는 공유 텍스처 크기(1024x1536, 6 MiB)로 그리는 동안 3개, 2초 조용하면 2개(`wanted_swapchain_buffers`), 숨기면 놓는다. 3개 18 MiB에 D3D와 DirectComposition 기본 몫을 더하면 맞는다. 바꿀 것을 찾지 못했다.
7. `dsa memory`의 "processes:"와 Google Play 줄이 늘 "0 resident"였다. 에이전트가 프로세스별 스왑을 `getMemoryStat("summary.total-swap")`로 읽었는데, 그것은 `getSummaryTotalSwap()`, 곧 공유 페이지까지 통째로 센 스왑 아웃 양이지 SwapPss가 아니다. 합이 781 MiB로 PSS 합 577 MiB보다 커서 상주분이 0이 되었다(`android.ext.services`는 PSS 35 MiB에 스왑 61 MiB, dumpsys의 TOTAL SWAP PSS는 25.8 MiB).

**바꾼 것.**

1. `dsa/graphics/patches/guest/mesa/0009-*.patch`: 프로세스의 ColorBuffer 참조에 보유자 수를 둔다. 버퍼를 들여온 기기 메모리와 창 버퍼 위에 만들거나 묶은 VkImage가 각각 보유자이고, 마지막 보유자가 사라질 때 `rcCloseColorBuffer`한다. 두 스레드가 같은 버퍼를 동시에 열면 남는 참조 하나를 닫는다. 시험: `dsa/graphics/test_window_buffer_patch.py`(`nfr1_`).
2. `HomeActivity`가 창을 1x1로 둔다(드래그 캐처 창과 같다). 입력 포커스를 받는 일은 그대로다.
3. hwcomposer: 2초 동안 층이 없는 태스크(최소화, 앱이 뒤로 감)는 합성 ColorBuffer 두 개를 놓고, 다음 프레임이 다시 만든다. 그때가 되면 SurfaceFlinger에 프레임을 청한다. 시험: `nfr1_a_task_without_layers_lets_go_of_its_buffers_only_after_a_while`.
4. 작업 집합 다듬기: 다듬기마다 그 뒤 되읽힌 양을 센다. 그것이 다듬은 양의 4분의 1에 이르면 다듬기가 작업 집합을 가져간 것이므로, 바닥을 "남긴 목표 + 되읽힌 양"으로 올린다. 바닥은 아무도 되읽지 않은 다듬기가 한 번 지나갈 때마다 64 MiB씩 내려가고, 앱이 닫히면 256 MiB로 돌아간다. 한 번만 읽힌 파일은 목표 위로 쌓이는 대로 다음 다듬기에 나간다. 시험: `nfr1_a_trim_whose_cache_comes_back_slowly_is_not_taken_again`.
5. 에이전트가 프레임워크의 숨은 `getSummaryTotalSwapPss()`(dumpsys의 TOTAL SWAP PSS)를 부르고, 어떤 프로세스도 PSS보다 많이 스왑 아웃했다고 세지 않는다. 호스트는 옛 에이전트가 보낸 합이 PSS 합보다 크면 그 합 대신 가장 큰 프로세스들의 몫(각각 PSS 이하)을 쓴다. 시험: `nfr2_no_process_counts_more_swapped_out_than_its_pss`(에이전트), `nfr2_a_swap_total_larger_than_the_pss_does_not_make_every_total_zero_resident`(호스트).

**설계상 기대치 (재지 않았다).**

| 항목 | 전 | 후 |
|---|---|---|
| 렌더러 GPU 할당, 창 하나로 1시간 | 148/11에서 475/47로 계속 늚 | 살아 있는 버퍼만큼에서 멈춤: 클라이언트 타깃 63, Play 창 버퍼 4개 23, 합성 타깃 12, ANGLE 내부 몫 |
| 홈 액티비티 버퍼 | 게스트 63 + 호스트 63 MiB | 1x1 두 개 |
| 최소화한 창의 합성 타깃 | 12 MiB | 2초 뒤 0 |
| 보이는 동안 다듬기 | 10-30초마다 72-147 MiB, 그 60% 되읽힘 | 되읽힌 것은 다시 다듬지 않고, 새로 읽힌 것만 나감 |
| `dsa memory` processes 줄 | 0 resident | 상주분이 보임 |

**확인하지 못한 것.**

- 위 모든 수치. 바뀐 게스트 드라이버, 에이전트, hwcomposer, channeld가 담긴 이미지로, 같은 조건(Play 스토어 창 하나, 최소화와 복원, 크기 바꾸기를 여러 번)에서 `dsa-wm.log`의 렌더러 줄이 늘지 않는지, `dumpsys SurfaceFlinger`의 `HomeActivity` 버퍼가 1x1인지, 안드로이드 로그의 `trimming` 줄 간격과 `workingset_refault_file` 증가가 줄었는지를 재야 한다. 렌더러의 할당 수를 자동으로 재는 시험은 실제 GPU와 안드로이드가 있어야 해서 이 변경에 없다.
- 창 버퍼의 참조를 VkImage가 사라질 때 닫는 것과 호스트가 그 이미지의 `vkDestroyImage`를 처리하는 순서. 둘은 다른 스트림이라 닫기가 먼저 닿을 수 있다. 그러면 호스트 이미지가 풀린 메모리에 묶인 채 곧 없어지는데, Vulkan에서는 쓰지 않는 한 허용된다. 다른 프로세스(SurfaceFlinger, hwcomposer)가 아직 쥔 버퍼는 그 참조로 남는다.
- 클라이언트 타깃 63 MiB. 줄이려면 SurfaceFlinger의 `FramebufferSurface`가 클라이언트 합성이 없을 때 버퍼를 놓게 하는 프레임워크 패치가 필요하다. 이 변경에 넣지 않았다.
- 1 MiB/s로 파일을 새로 읽는 프로세스. root 셸이나 `/proc/<pid>/io`를 읽을 권한으로 가려야 한다.

### 유휴 기준선: 커널과 상주 서비스가 잡는 것, 줄인 것 (2026-11-11)

**기준.** 소유자 PC(Windows 11 Pro 26300, x64, 32 GiB), v0.2.2, Android 17 x86_64 이미지(`CP2A.260605.016`, 증분 `16373615.dsa79c0f759bacb`), Google Play 설치, 부팅 뒤 1시간 16분, VM 3072 MiB와 프로세서 4개. 잴 때 Play 스토어 창 하나가 열려 있었고 다른 앱 하나(`skplanet.musicmate`)가 캐시로 남아 있었다. 빌드하거나 실행하지 않았고 바탕 화면은 건드리지 않았다. 읽은 것: `dsa memory`, adb의 shell 사용자로 `dumpsys meminfo`, `/proc/meminfo`, 모든 프로세스의 `/proc/<pid>/status`, `/proc/mounts`, `logcat -d -b kernel`(부팅 때의 커널 메시지), `getprop`, `pm list features`, 이미지의 init 스크립트. user 빌드의 shell은 `/proc/vmallocinfo`, zram의 sysfs 파일, `/sys/kernel/slab/*`의 속성을 읽지 못하고, 이 커널에는 `/proc/slabinfo`가 없다. 그래서 slab과 vmalloc은 합계만 알고 캐시별 내역은 모른다. 이미지의 `su`는 쓰지 않았다(호스트가 바탕 화면에 승인 창을 띄운다).

**잰 것.**

| 항목 | 값 | 출처 |
|---|---|---|
| 호스트 합 | 1074 MiB: vm 상주 749(private 942), 창 관리자 29, 렌더러 260, 앱 창 36 | `dsa memory` |
| 게스트 사용 | 823 / 2975 MiB: 페이지 캐시 214, 앱(익명) 127, 커널과 나머지 456 | `dsa memory` |
| 커널과 나머지 456의 내역 | 회수 못 하는 slab 100, 커널 스택 21, 페이지 테이블 40, vmalloc 60, per-CPU 6, zram 저장소 164, 이름 없는 것 64 | `dsa memory` |
| zram | 압축 전 478 MiB를 164 MiB에 보관(2.91배, LZ4), 크기 1488 MiB | `dsa memory` |
| 부팅 때 커널이 떼어 둔 것 | 97 MiB. 커널 이미지 41 MiB(code 20873K, rwdata 2958K, rodata 14904K, bss 2732K), 3 GiB의 페이지 구조체 48 MiB(786079 페이지 × 64 B), swiotlb 4 MiB, ftrace 레코드 0.9 MiB(59032개, 232 페이지) | 커널 부팅 메시지 `Memory:`, `ftrace:` |
| 스레드 | 1347개(커널 스택 16 KiB씩 = 21 MiB). 커널 스레드 230개: ext4-rsv-conversion 구조 스레드 75개(APEX의 ext4 마운트마다 하나), kverityd 고우선 워커 약 40개, kdmflush 17개. 사용자 스레드 1117개: system_server 252, Play 스토어 113, Play 서비스 86, musicmate 67, Play 스토어 배경 63, 에이전트 39 | `/proc/*/status` |
| 페이지 테이블 | 사용자 프로세스 98개의 VmPTE 합 39.3 MiB. system_server 2.4, Play 서비스 2.0, Play 스토어 2.0. HAL과 네이티브 서비스 32개는 평균 0.19 MiB(스레드 평균 1.7개) | `/proc/*/status` |
| 마운트 | ext4 170개(APEX 약 75개가 loop와 dm-verity 위의 ext4, 각각 두 번), erofs 18개, overlay 14개 | `/proc/mounts` |
| DMA-BUF | 230 MiB(거의 모두 매핑되지 않음), Play 스토어 창이 열린 동안 | `dumpsys meminfo` |

PSS(`dumpsys meminfo`, zram에 있는 몫을 압축 전 크기로 센다): Play 스토어 178(창이 열림), system_server 131, Play 서비스 73, musicmate 47, 에이전트 32, android.ext.services 30, Play 스토어 배경 28, Play 서비스 unstable 18, MediaProvider 15, SurfaceFlinger 14, NetworkStack 12, 설정 11, partnersetup 10, phone 8.5, audioserver 7.0, webview_zygote 6.9, media.swcodec 6.4, zygote64 6.0 MiB. 그 밖의 네이티브 서비스와 HAL 약 70개가 각각 0.6-4 MiB다. `dsa memory`가 세는 실제 비용(상주와 zram 몫)으로는 system_server 76(상주 39, zram 37), Play 서비스 27, 에이전트 12 MiB다.

**HAL 프로세스 하나의 비용.** PSS 0.77-1.4 MiB(잰 값), 페이지 테이블 약 0.19 MiB, 스레드 1-3개의 커널 스택 16-48 KiB, 그리고 task, mm, VMA, binder 객체의 slab(재지 못했다, 수백 KiB로 본다). 하나에 약 1.2-1.8 MiB다.

**바꾼 것과 기대하는 절감.**

1. 하드웨어가 없는 예제 HAL 13개(APEX: authsecret, rebootescrow, cas, contexthub, input.processor, ir, memtrack, net.nlinterceptor, tetheroffload, neuralnetworks의 샘플 드라이버 셋, secure_element, usb, vibrator)와 가짜 얼굴·지문 HAL 둘(평범한 vendor 파일)을 이미지에서 뺐다. 얼굴, 지문, 적외선, 컨텍스트 허브 기능도 `unavailable-features.xml`로 거뒀다. 기능이 남아 있으면 system_server가 생체 인증 서비스와 컨텍스트 허브 서비스를 시작해 HAL을 기다리고, 적외선 서비스는 기능이 있는데 HAL이 없으면 시작을 거부해 system_server가 죽는다. 이 17개 프로세스의 PSS 합은 16.3 MiB였다(얼굴 1.2, 지문 0.85, NNAPI 샘플 셋 4.0, 나머지 0.77-1.13씩). 페이지 테이블 약 3.3 MiB, 커널 스택 약 0.5 MiB, slab 약 2-4 MiB를 더해 게스트에서 약 20-24 MiB를 기대한다. PSS의 일부는 이미 zram에 압축되어 있으므로 호스트가 실제로 아끼는 것은 약 15-20 MiB로 본다(재지 않았다). 남긴 것: thermal(게임의 열 여유 API), drm.clearkey(ClearKey 콘텐츠), identity, light, wifi, boot, 보안 HAL들, motioncontext(무엇에 쓰는지 확인하지 못해 남겼다). 커밋 `89590e3a`.
2. zram에 두 번째 압축기로 zstd를 두었다(`CONFIG_ZRAM_MULTI_COMP`, `recomp_algorithm`을 `swapon_all` 전에). 유휴 회수가 차가운 페이지를 zram으로 보낸 뒤 zram의 페이지를 모두 idle로 표시하고, 회수마다 최대 64 MiB(16384 페이지)를 zstd로 다시 압축하고, zram을 압축 정리한다. 스왑마다 쓰는 압축기는 LZ4 그대로라서 앱을 쓰는 동안의 스왑 속도는 바뀌지 않는다. 이미 다시 압축한 페이지는 건너뛰고, 다시 읽힌 페이지는 zram을 떠나므로 차갑게 남는 것만 zstd가 된다. 안드로이드 익명 메모리에서 zstd는 LZ4보다 약 20-30% 더 작게 압축한다고 보고, 164 MiB가 약 125-135 MiB가 되어 약 30-40 MiB를 기대한다(재지 않았다). 비용: zstd의 압축 스트림(프로세서마다, 합쳐 1-2 MiB로 본다), 회수 한 번에 init이 0.3초 안팎 바쁜 것, zstd로 압축된 페이지를 다시 읽을 때 LZ4보다 몇 배 느린 해제(페이지당 수 마이크로초). 커밋 `f2f6b4ac`.
3. config-wsl이 WSL 배포판을 위해 내장하는 것 가운데 게스트가 쓰지 않는 것을 뺐다: XFS, squashfs, ISO 9660, UDF, ext2(ext4가 대신 붙인다), NVMe, RAM 디스크, KSM, kallsyms의 변수 이름(`KALLSYMS_ALL`, 라이브 패치가 함께 빠진다). 커널 이미지는 VM이 도는 내내 상주하므로 그만큼이 모든 게스트에서 준다. 약 3 MiB로 본다(대부분 XFS, 재지 않았다). 남긴 것: BTF(netbpfload가 맵을 BTF와 함께 만든다, 부팅 로그 `btf:1`), ftrace(perfetto와 atrace), vfat, f2fs, erofs. 커밋 `88480b1a`.
4. CI가 릴리스마다 수치를 남긴다. Image test에 boot check를 하나 더 두었다("Measure the memory with Google Play and one app window on the booted image"): LiteGapps의 빌드로 Google Play를 설치하고 다시 부팅한 뒤 설정 창 하나를 열고, 1분 손대지 않은 뒤 `dsa memory`와 같은 방법으로 10초 간격 여섯 번 읽는다. crosvm 프로세스의 상주·private, 창 관리자, 렌더러, 앱 창, 서비스, 게스트의 사용, 커널 항목, zram, 프로세스 비용(상주와 zram 몫), Google Play 서비스의 몫, `dumpsys meminfo`의 프로세스별 PSS를 중앙값으로 job summary와 `memory.json`(artifact `memory-measurement`)에 적는다. 기준선(`dsa/host/platform/linux/tests/memory_check/baseline.json`)보다 4분의 1과 100 MiB를 모두 넘게 늘었을 때만 실패한다. 기준선은 아직 비어 있다: 첫 develop 실행의 `memory.json`의 `baseline` 값을 그 실행과 이미지와 함께 옮겨 적어야 비교가 시작된다. 이 수치는 Linux 러너(KVM 위 crosvm, 렌더러는 lavapipe)의 것이라 Windows의 `vmmem`과 같지 않다. 같은 이미지끼리의 회귀를 잡는 용도다.

**바꾸지 않은 것과 그 이유.**

- VM 시작 크기 3072 MiB. 페이지 구조체가 48 MiB이고 VM이 도는 내내 상주한다. 2048 MiB로 시작하면 16 MiB가 준다. SPEC NFR-2가 시작 크기를 3 GiB로 정하고 있어서 바꾸지 않았다. 소유자에게 제안한다: 비용은 Google Play가 있는 부팅 직후(게스트 사용 1.4-1.8 GiB, 위 R26의 표)에 핫애드가 한두 번 일어나는 것이고, 핫애드된 메모리의 페이지 구조체는 그 메모리 안에 놓이므로(`MHP_MEMMAP_ON_MEMORY`) 시작 크기 밖의 비용은 늘지 않는다.
- zram 크기(메모리의 절반, 1488 MiB). zram의 슬롯 표가 슬롯마다 16 B, 약 6 MiB의 vmalloc이다. 절반으로 줄이면 3 MiB가 주지만 압축해 둘 수 있는 양도 준다.
- zram writeback(디스크로 내보내기). 뒷받침 장치가 있어야 하고, 쓰기는 사용자의 디스크를 닳게 한다. Android 17의 mmd가 writeback과 재압축을 하지만 이 빌드에서는 aconfig 플래그가 꺼져 있어(`mmd.enabled_aconfig`가 없다) `mmd_setup`이 아무것도 하지 않는다. aconfig 플래그를 켜려면 이미지의 플래그 저장소를 바꿔야 한다. 재압축은 위 2로 대신했다.
- ART 힙. 이미 1 GiB 휴대전화의 값이다(`heaptargetutilization` 0.75, `heapminfree` 512k, `heapmaxfree` 8m). 더 낮추면 GC가 잦아지는 데 비해 줄어드는 몫은 프로세스마다 수 MiB 이하다.
- zygote 프리로드. zygote64는 PSS 6 MiB(RSS 19 MiB)이고 그 페이지를 모든 앱이 나눠 쓴다. 프리로드를 줄이면 그 비용이 앱마다 따로 생긴다. Google Play 서비스의 프로세스 다섯과 시스템 앱이 늘 있으므로 지금이 낫다.
- webview_zygote(PSS 6.9 MiB). WebViewUpdateService가 부팅 때 바로 시작한다. 늦게 시작하게 하려면 프레임워크를 고쳐 빌드해야 하고, 그것은 소유자에게 먼저 물어야 한다.
- com.android.phone(PSS 8.5 MiB, 그중 대부분 zram). 영구 프로세스라 system_server가 바로 다시 띄운다. 전화 기능은 이미 거뒀다. 없애려면 프레임워크 변경이 필요하다.
- ext4-rsv-conversion 구조 스레드 75개(약 2 MiB). 읽기 전용 ext4 마운트에도 하나씩 생긴다. 읽기 전용일 때 만들지 않게 하는 커널 패치는 다시 쓰기로 붙일 때를 함께 다뤄야 해서 이번에 하지 않았다.
- swiotlb(4 MiB), KFENCE 풀(끔), dentry·inode·TCP·UDP 해시 표: 이미 커널 명령줄로 줄여 두었다.
- 렌더러의 GPU 메모리, 창을 보일 때 늘어나는 페이지 캐시, 앱 창 프로그램의 버퍼, `dsa memory`의 프로세스 줄은 다른 작업이 맡았다. 위 DMA-BUF 230 MiB도 창이 열린 동안의 버퍼라 그쪽에 속한다.

**확인하지 못한 것.**

- 위 1-3의 절감은 모두 기대값이다. 이 PC에서 빌드하거나 부팅하지 않았다. Image test와 Kernel test가 develop에서 이미지와 커널을 만들고 부팅한다. 특히 SELinux가 enforcing일 때 `vendor_init`이 zram의 sysfs 파일(`recomp_algorithm`, `idle`, `recompress`, `compact`)에 쓸 수 있는지는 부팅 로그의 거부 줄로 확인해야 한다.
- slab 100 MiB와 vmalloc 60 MiB의 캐시별 내역. root로 `/sys/kernel/slab`과 `/proc/vmallocinfo`를 읽어야 한다. VMA(`vm_area_struct`, `maple_node`), SELinux 정책(`avtab_node`), dm-bufio(파티션과 APEX의 dm-verity 해시 블록), binder를 의심한다.
- CI의 첫 측정값과 기준선. 기준선을 적기 전에는 비교하지 않는다.
- 뺀 HAL을 앱이 찾는 경우(진동, NNAPI 가속, IR)에 앱이 하드웨어가 없는 기기처럼 동작하는지. 실기에서 확인해야 한다.

### 소유자가 승인한 네 항목 (2026-11-11)

**기준.** 위 유휴 기준선과 같은 PC와 이미지에서 잰 값을 출발점으로 삼았다. 이 변경에서 빌드하거나 실행하지 않았고, 아래 절감은 모두 기대값이다. 프레임워크 원본은 framework-lock.json이 고정한 커밋(frameworks/native `fcbde2bc`(16), `ae266dcb`(17), frameworks/base `94b4c163`(17), packages/services/Telephony `4a3830d9`(17))에서 읽었다.

1. SurfaceFlinger의 클라이언트 타깃. HWC3에는 클라이언트 타깃 크기를 정하는 API가 없다(`ClientTargetProperty`는 픽셀 형식과 데이터 공간뿐이다). `ro.surface_flinger.max_graphics_width/height`로 줄이면 SurfaceFlinger가 줄인 버퍼에 그리므로, 클라이언트로 합성되는 YUV 영상의 해상도가 떨어진다. 그래서 크기는 그대로 두고 쓰지 않을 때 놓게 했다. frameworks/native 0004(16, 17): 클라이언트 합성이 없는 프레임이 마지막 클라이언트 합성 프레임보다 1초 뒤이면, 그 프레임이 표시된 뒤 `FramebufferSurface`가 현재 버퍼를 큐에 돌려주고 `discardFreeBuffers`로 큐의 버퍼를 모두 해제한다(생산자 쪽 `Surface`도 `onBuffersDiscarded`로 놓는다). `RenderSurface`는 그 버퍼들의 텍스처 캐시를 비운다. 다음 클라이언트 합성 프레임이 새 버퍼를 할당하고 HWC에 통째로 보낸다. hwcomposer는 마지막 클라이언트 합성 2초 뒤 클라이언트 타깃 ColorBuffer의 호스트 참조를 닫고, 그때 SurfaceFlinger에 프레임을 청한다. 그 프레임에서 SurfaceFlinger도 버퍼를 놓는다. 스크린샷과 screencap은 자기 버퍼에 그리므로 영향이 없다. 기대: 클라이언트 합성이 없는 동안 렌더러 GPU 메모리 약 63 MiB(2880x2880 RGBA 두 개). 비용: 영상이 다시 나올 때 첫 프레임에 버퍼 두 개를 할당하는 수 ms. 시험: `nfr1_the_client_target_is_let_go_after_a_while_without_client_composition`(hwcomposer), `test_nfr1_both_versions_free_the_client_target_when_nothing_is_composed_into_it`(패치). 패치는 Framework build를 거쳐야 이미지에 들어간다. 이 변경에서는 두 패치가 고정 커밋의 원본에 그대로 적용되는지(`apply --check`)까지만 확인했다.
2. VM 시작 크기 3072에서 2048 MiB로(`DEFAULT_MEMORY_MB`, Windows, Linux, macOS 공통). 늘리는 규칙은 그대로이고(한 번에 현재 크기의 절반, 최소 1 GiB), 2 GiB에서 처음 늘면 3 GiB가 된다. 기대: 부팅 메모리의 페이지 구조체가 48에서 32 MiB로, 16 MiB 준다. 비용: Google Play가 있으면 부팅 직후 핫애드가 한두 번 일어난다. 시험: `nfr2_the_vm_boots_at_two_gib_and_grows_from_there`, 그리고 Linux와 macOS의 기존 `nfr2_` 시험을 2048에 맞췄다.
3. com.android.phone. TeleService의 `PhoneGlobals.onCreate`는 `FEATURE_TELEPHONY`가 없으면 바로 돌아가고, `TelecomAccountRegistry`도 만들어지지 않는다. 그래서 phone, isub, 통신사 설정 서비스는 지금도 게시되지 않고, system_server는 이미 그것 없이 돈다. TeleService가 없는 Android TV 빌드와 같다. 이미지에서 TeleService를 뺐다(`UNNEEDED_APPS`). Telecom(VoIP 앱이 쓰는 ConnectionService)과 telephony provider(같은 프로세스 이름으로, 앱이 물을 때만 뜬다)는 남긴다. 기대: PSS 8.5 MiB(그중 대부분 zram), 호스트에서 약 3-5 MiB. 시험: `test_nfr1_the_phone_process_is_not_in_the_image`. Framework build는 필요 없다.
4. webview_zygote를 처음 쓸 때 시작하기. 하지 못했다. `WebViewUpdateServiceImpl2`가 provider를 준비한 뒤 `SystemImpl.ensureZygoteStarted()`로 `WebViewZygote.getProcess()`를 바로 부르고, 이를 막는 설정, 시스템 속성, aconfig 플래그가 없다. 부팅 뒤 webview_zygote를 끝내면 system_server가 죽은 소켓을 쥔 채 남아 다음 렌더러 시작이 실패하고, 재부팅 전까지 WebView가 열리지 않는다. 그래서 services.jar를 고쳐야 한다(`ensureZygoteStarted`가 아무것도 하지 않으면, 첫 렌더러 시작 때 `ProcessList`가 zygote를 띄우고 그 뒤로 남는다). 지금 Framework build는 SurfaceFlinger, libhwui, libEGL, KeyMint만 만들고 frameworks/base의 일부만 받으므로 services.jar를 만들지 못한다. 방법은 둘이다. (가) Framework build가 services.jar를 만들고, 이미지가 그것을 시스템 서버 클래스 경로에 맞게 다시 컴파일한다. 받을 트리와 빌드 시간이 크게 는다. (나) make-image.py가 앱의 dex를 고치는 방식(`edit_dex`)으로 services.jar의 그 호출 하나를 지우고 services.jar를 다시 컴파일한다. 시스템 서버 jar를 다시 컴파일하는 경로는 아직 없다. 어느 쪽이든 system_server의 코드를 바꾸므로 소유자가 고른 뒤 한다. 기대: webview_zygote PSS 6.9 MiB(WebView를 쓰기 전까지). 비용: 첫 WebView가 zygote 시작(수백 ms)을 기다린다.

**확인하지 못한 것.** 위 수치 모두. 1은 Framework build로 만든 SurfaceFlinger가 담긴 이미지에서 영상을 틀었다 끄고 2초 넘게 지난 뒤, `dumpsys SurfaceFlinger`의 FramebufferSurface 슬롯이 비고, hwcomposer 로그에 "let go of the client target" 줄이 남고, `dsa-wm.log`의 렌더러 메모리가 약 63 MiB 주는지 봐야 한다. 영상을 다시 틀 때 첫 프레임이 깨지지 않는지도 봐야 한다. 2는 2 GiB 부팅과 Google Play 첫 부팅의 핫애드를 Image test와 실기에서 봐야 한다. 3은 TeleService 없이 부팅해 system_server가 telephony 때문에 죽지 않는지, 설정의 휴대전화 정보 화면이 열리는지를 Image test의 로그로 봐야 한다.

### zram writeback과 빈 페이지 다시 보고 (2026-11-11, v0.2.4)

**목표.** 소유자 PC의 zram 저장소는 압축된 익명 메모리 100-160 MiB를 쥐고 있다(위 기준선: 압축 전 478 MiB를 164 MiB에). NFR-1의 앱 하나 250 MiB에 다가가려면 Doze 동안 차갑게 남는 이 몫을 메모리 밖으로 내보내고, 게스트가 푼 메모리가 더 빨리, 더 작은 조각으로 호스트에 돌아가 VM 상주와 게스트 사용의 차이가 줄어야 한다.

**방법.** 빌드하거나 실행하지 않았고 바탕 화면과 설치된 DSA는 건드리지 않았다. 읽은 것: 고정 커널(`linux-msft-wsl-6.18.54.1`)의 `drivers/block/zram/zram_drv.c`, `Documentation/admin-guide/blockdev/zram.rst`, `mm/page_reporting.c`, `drivers/hv/hv_balloon.c`, AOSP `system/core`의 `fs_mgr/fs_mgr.cpp`(`PrepareZramBackingDevice`, `ShouldSkipZramSetup`)와 `fs_mgr/libfstab/fstab.cpp`, `init/builtins.cpp`, Android 16 target_files의 `plat_sepolicy.cil`, `plat_file_contexts`, `plat_property_contexts`. 내려받은 것은 `.scratch/zram-src/`에 있다.

**찾은 것.**

1. zram writeback은 이 커널에 있다(`CONFIG_ZRAM_WRITEBACK`). 뒷받침 장치는 디스크 크기를 정하기 전에 `backing_dev`에 이름을 써야 하고, 그 뒤 `writeback`에 `huge`, `idle`, `huge_idle`, `incompressible`, `page_index(es)=`를 쓴다(6.16부터 `type=` 꼴도 받는다). `writeback`에는 `recompress`의 `max_pages` 같은 상한이 없다. 상한은 `writeback_limit_enable`과 `writeback_limit`(4 KiB 단위로, 쓸 때마다 줄어 0이면 `-EIO`로 멈춘다)뿐이다. writeback은 페이지를 압축을 풀어 4 KiB 그대로 동기 쓰기 한 번씩으로 쓴다. 그래서 압축된 페이지 하나를 쓰면 메모리는 그 압축 크기(약 1.3 KiB)만큼만 줄고, 압축되지 않은 페이지(`huge`)는 4 KiB가 준다.
2. `idle`에 초 수를 쓰면(`CONFIG_ZRAM_TRACK_ENTRY_ACTIME`) 그보다 오래 쓰이지 않은 페이지만 idle로 표시하고, 6.18의 `mark_idle`은 그보다 젊은 페이지의 표시를 지운다. 그런데 재압축(`recompress_slot`)은 `zram_free_page`를 부르고, 그 함수가 `ac_time`을 0으로 만든다. PR #622가 회수마다 zstd로 다시 압축하므로, 그대로라면 재압축된 페이지는 모두 부팅 뒤 한 번도 쓰이지 않은 것으로 보여 나이와 상관없이 디스크로 나간다. 재압축은 사용이 아니므로 그 시각을 지키는 커널 패치를 두었다.
3. fs_mgr는 fstab의 `zram_backingdev_size=`가 있으면 `swapon_all` 안에서 `/data/per_boot/zram_swap`을 열고(`O_CREAT`), `fallocate`로 크기를 잡고, loop 장치에 direct I/O로 붙여 `backing_dev`에 쓴다. 실패하면 로그만 남기고 스왑은 뒷받침 없이 켠다. `swapon_all`은 vendor 스크립트에서도 init 자신이 실행한다(`builtins.cpp`의 subcontext 표시가 false). `/data/per_boot`는 AOSP `init.rc`가 부팅이 끝날 때(`on property:sys.boot_completed=1`) 지난 폴더를 toolbox의 `rm -rf`로 지우고 `mkdir /data/per_boot 0700 system system encryption=Require key=per_boot_ref`로 다시 만든다. zram은 그보다 먼저(`on boot`) 켜지므로 그때는 폴더가 없다. 그 경로의 라벨은 `plat_file_contexts`에 `system_data_file`로만 있다. 폴더가 이미 다른 키로 있으면 `init`의 `make_dir_with_options`가 `FscryptSetDirectoryPolicy`에 실패해 `reboot_into_recovery --prompt_and_wipe_data`로 사용자 데이터 삭제를 묻는다(2026-11-12 검토에서 확인, `system/core` main의 `init/builtins.cpp`). 폴더는 init 자신만 만들 수 있다(vendor 스크립트의 `mkdir`은 `vendor_init`이 실행한다). loop 드라이버는 커널 스레드에서 파일을 읽고 쓰므로 `kernel`에 그 파일의 읽기와 쓰기가 있어야 한다.
4. Android 17의 mmd는 zram 설정, 재압축, writeback을 맡을 수 있지만 aconfig 플래그로 켜진다. fs_mgr의 `ShouldSkipZramSetup`은 `mmd.enabled_aconfig`가 `true`일 때만 `swapon_all`의 zram 설정을 건너뛰고, 이 이미지에는 그 속성이 없다(위 기준선 조사). mmd를 쓰려면 이미지의 aconfig 플래그 저장소를 바꾸고 mmd의 정책과 속성(`ro.zram.mark_idle_delay_mins`, `ro.zram.periodic_wb_delay_hours` 등)을 맞춰야 하는데, 그러면 writeback 시점을 DSA가 아는 것(앱이 열려 있는지, Doze인지)에 맞출 수 없다. 그래서 dsa-channeld가 결정하고 init이 쓰는 지금의 길을 따랐다.
5. 빈 페이지 보고는 이미 순서 0(4 KiB, 작은 백킹 페이지를 쓸 수 있는 Windows와 Linux)이고 지연이 없다(`page-reporting-no-delay.patch`). 남은 틈은 둘이다. 첫째, per-CPU 목록에 있는 빈 페이지는 버디 목록에 없어 보고되지 않는다. 둘째, Hyper-V는 VM 메모리를 64 KiB 클러스터로 들여온다(`FaultClusterSizeShift` 4). 보고된 4 KiB 페이지 옆의 페이지를 게스트가 다시 쓰면 클러스터 전체가 다시 커밋되는데, 게스트는 그 안의 다른 빈 페이지를 이미 보고한 것으로 표시해 두고 다시 내놓지 않는다. 그 페이지는 할당되었다가 다시 풀릴 때까지 VM에 남는다. 위 v0.2.2의 표에서 vm private가 게스트 사용보다 130-310 MiB 큰 것의 일부가 이것이라고 본다(가설이고, 재지 않았다). `vm.rs`의 주석("보고 순서는 클러스터 이상이어야 한다")과 지금의 순서 0이 어긋나는 것도 같은 문제다. 순서를 4로 올리면 클러스터와 맞지만 64 KiB 블록이 되지 않은 빈 조각은 돌아가지 않고, 클러스터를 4 KiB로 줄이는 것은 HCS가 받는지 확인하지 못했고 처음 쓰는 페이지마다 폴트가 16배가 된다. 그래서 순서와 클러스터는 두고, 회수 뒤에 모든 빈 페이지를 다시 보고하게 했다.
6. 호스트는 vmmem의 작업 집합을 다듬지 않는다(위 v0.2.2 조사). vmmem은 보호된 최소 프로세스이고, 다듬으면 게스트 페이지가 페이지 파일로 나가 다시 읽을 때 디스크를 읽는다. 호스트가 다듬는 것은 창 관리자와 렌더러이고, 유휴 구간마다 한 번뿐이었다.

**바꾼 것.**

1. 커널: `CONFIG_ZRAM_WRITEBACK`, `CONFIG_ZRAM_TRACK_ENTRY_ACTIME`. `patches/zram-recompress-keeps-access-time.patch`(재압축이 `ac_time`을 지킨다). `patches/page-reporting-rereport.patch`: 쓰기 전용 매개변수 `page_reporting.rereport`에 1을 쓰면 per-CPU 목록을 비우고(`drain_all_pages`), 보고 순서 이상의 빈 페이지에서 보고 표시를 지우고, 보고를 한 번 돌린다. 압축 정리 뒤의 빈 메모리는 큰 블록 몇 개라서 보고 횟수는 적다.
2. 이미지: fstab의 zram 줄에 `zram_backingdev_size=512M`. 새 system 스크립트 `dsa-zram-backing.rc`가 `post-fs-data`에서 `init.rc`와 똑같이 지난 부팅의 폴더를 지우고 같은 소유자와 그 부팅만의 키(`per_boot_ref`)로 다시 만든다. 처음 판은 폴더를 root 소유로, 기본 키로 만들었는데, 그러면 부팅이 끝날 때 `init.rc`의 `rm -rf`(uid system)가 폴더를 지우지 못하고 이어지는 `mkdir`이 키가 달라 데이터 삭제를 묻는다. 그래서 병합 전에 고쳤다. `init.rc`가 부팅 끝에 지우는 뒷받침 파일은 loop가 열어 두므로 Android가 멈출 때까지 그대로 쓰인다. 스왑된 페이지는 그 부팅의 키로만 암호화되어 부팅을 넘어 남지 않고, fs_mgr가 새로 잡는다. toolbox의 `rm`에는 그 폴더를 비우고 지우는 권한만 주었다(파일을 열 수는 없다). `memory.cil`에 `dsa_zram_backing_file`(init이 만들고, 커널이 읽고 쓰고, 다른 도메인은 열 수 없다)과 `rereport` 파일의 라벨, `plat_file_contexts`에 폴더의 라벨. `dsa-memory.rc`는 스왑을 켠 뒤 `writeback_limit_enable`을 켜고 예산은 주지 않는다. 그래서 dsa-channeld가 묻기 전에는 아무것도 쓰지 않는다.
3. dsa-channeld(`writeback.rs`): 30초마다 본다. 앱이 하나도 쓰이지 않은 지 10분이 지나고, 지난 실행에서 10분이 지났고, zram이 8 MiB 넘게 쥐고 있으면 그 실행의 예산(32 MiB와 오늘 남은 예산 중 작은 것)을 주고 init에 실행을 맡긴다. init은 예산을 주고, 30분 넘게 쓰이지 않은 페이지만 idle로 표시하고, 압축되지 않은 것부터(`huge_idle`) 그 다음 나머지(`idle`)를 쓰고, 예산을 0으로 되돌리고, zram을 압축 정리하고, 빈 페이지를 다시 보고한다. 데몬은 `bd_stat`의 쓴 양을 오늘의 몫에 더해 영구 속성(`persist.vendor.dsa.memory.writeback_day`)에 둔다. Android가 다시 시작해도 그날의 예산은 다시 생기지 않는다. 줄마다 쓴 양, 걸린 시간, zram 저장소의 전후, 디스크에 있는 양, 남은 예산을 로그에 남긴다. zram에 뒷받침 장치가 없으면 이유를 한 번 남기고 멈춘다. zstd 재압축은 그대로 회수마다 먼저 돈다.
4. 유휴 회수(앱이 닫힐 때와 창이 없을 때)의 끝에 `page_reporting.rereport`를 쓴다.
5. 창 관리자의 유휴 다듬기는 창이 없는 동안 5분마다 다시 돈다(`IDLE_TRIM_REPEAT`). Doze 동안에도 작업과 메모리 이벤트, 알림이 창 관리자의 작업 집합을 다시 채우는데, 지금까지는 다음 창이 열렸다 닫힐 때까지 그대로였다.
6. CI의 메모리 측정(PR #625): 창을 연 상태의 읽기에 "VM이 게스트 사용보다 더 쥔 것"(상주와 private, 중앙값)을 더했다. 그 다음 창을 닫고, 부팅 명령줄로 줄인 시간(앱 없이 30초 뒤, 60초 넘게 안 쓰인 페이지, `androidboot.dsa.zram_writeback_after`, `androidboot.dsa.zram_writeback_age`)으로 writeback을 기다린 뒤 다시 세 번 읽는다. job summary의 "After the window closes" 표에 창이 열렸을 때와 닫힌 뒤의 VM 상주, VM private, 게스트 사용, zram 저장소, zram의 압축 전 크기, 그리고 VM이 게스트 사용보다 더 쥔 것을 나란히 적고, 게스트 로그에서 읽은 writeback의 쓴 양과 저장소의 전후를 적는다. zram에 뒷받침 장치가 없으면 실패하고, 기다리는 동안 writeback이 돌지 않았으면 경고만 한다.

**SSD 쓰기 상한.** 한 번에 32 MiB(8192 페이지), 실행 사이 10분 이상, 하루 512 MiB(131072 페이지). 앱이 10분 넘게 쓰이지 않을 때만, 30분 넘게 쓰이지 않은 페이지만 쓴다. 뒷받침 파일은 512 MiB이고 `fallocate`라 처음에는 아무것도 쓰지 않는다. 한 번 쓴 페이지는 다시 읽힐 때까지 디스크에 있으므로 첫 긴 Doze 뒤에는 새로 차가워진 것만 쓴다. 매일 상한까지 써도 1년에 약 183 GiB로, 300 TBW SSD의 약 0.06%다. 32 MiB는 4 KiB 동기 쓰기 8192번이라 init이 1초 안팎 바쁜 것으로 본다(재지 않았다).

**기대하는 절감(재지 않았다).** 창 없는 Doze가 한두 시간 이어지면 zram 저장소 100-160 MiB 가운데 차갑게 남는 몫(Play 서비스의 SwapPss가 대부분이다)이 디스크로 나가, 저장소가 약 50-100 MiB 준다고 본다. 하루 예산 512 MiB는 압축비 약 3에서 메모리 약 170 MiB에 해당하므로 첫날 상한은 그것이다. 다시 보고는 per-CPU 목록의 몫(수 MiB)과 Hyper-V 클러스터로 다시 커밋된 빈 페이지를 돌려준다. 후자는 위 private와 게스트 사용의 차이(130-310 MiB)의 일부일 것으로 보지만 크기는 모른다. 창 관리자의 반복 다듬기는 수 MiB다.

**확인하지 못한 것.**

- 위 수치 모두. 이미지와 커널은 develop의 Image test와 Kernel test가 만든다. 메모리 측정의 "After the window closes" 표와 writeback 줄이 첫 수치다. CI는 KVM 위 crosvm이라 Hyper-V의 64 KiB 클러스터가 없고, 다시 보고의 효과는 per-CPU 목록 몫만 보인다. Windows에서는 Doze 몇 시간 뒤 `dsa memory`로 vm 상주, private, 게스트 사용을 견줘야 한다.
- SELinux가 enforcing일 때 fs_mgr가 `/data/per_boot/zram_swap`을 만들고 loop에 붙이는지, loop 드라이버의 읽기와 쓰기가 거부되지 않는지, `vendor_init`이 `rereport`와 `writeback`, `writeback_limit`에 쓸 수 있는지. 부팅 로그의 `avc` 줄과 dsa-channeld의 "zram has no backing file" 줄로 확인한다. 메모리 측정은 후자가 있으면 실패한다.
- `ro.boot.dsa.*`를 `vendor_init`이 읽어 `setprop`에 펼치는지. `dsa-display.rc`의 `ro.boot.lcd_density`와 같은 길이지만, 이 이름에 따로 확인하지 않았다.
- 앱이 다시 열 때 디스크에서 읽어 들이는 지연. writeback된 페이지는 VHDX 위 ext4 위 loop에서 동기로 읽힌다. 30분 넘게 쓰이지 않은 페이지만 내보내므로 드물다고 보지만, Play 스토어처럼 오래 닫혀 있다가 여는 앱의 첫 화면이 느려지는지 실기에서 봐야 한다.
- `rereport`가 Hyper-V의 이미 돌려받은 범위를 다시 받는 비용. hv_balloon은 범위마다 hypercall 묶음 하나이고 압축 정리 뒤의 빈 메모리는 큰 블록이라 적다고 본다.

### 커널 slab의 내역을 root로 읽기, 커널과 system_server 줄이기 (2026-11-11, v0.2.4 대상)

**기준.** 위 유휴 기준선의 수치(PR #622와 #626, 소유자 PC, v0.2.2, Google Play)가 출발점이다. 큰 몫은 회수 못 하는 slab 100, vmalloc 60, 페이지 테이블 40, 커널 스택 21 MiB(스레드 1347개, 그중 ext4-rsv-conversion 75개), system_server PSS 131 MiB(실제 비용 76 MiB)다. 이 절의 작업은 브랜치 `feat/slab-system-server`에서 했고, 이 PC에서 빌드, 시험, 부팅을 하지 않았다. 읽은 소스: WSL 커널 `linux-msft-wsl-6.18.54.1`의 `arch/x86/configs/config-wsl`, `fs/ext4/super.c`, `fs/ext4/page-io.c`(GitHub의 원본을 `.scratch/`에 받아 읽었다), 이미지의 Android 16 `plat_sepolicy.cil`(`proc_slabinfo`, `proc_vmallocinfo`의 레이블, shell의 `debug_prop` 설정 권한, MLS 제약).

**slab을 캐시별로 읽지 못했던 까닭.** 둘이 겹쳤다. config-wsl이 `CONFIG_SLUB_DEBUG`를 꺼서 `/proc/slabinfo`가 없고 `/sys/kernel/slab/*`에는 객체 수와 slab 수가 없다. 있어도 `/proc/slabinfo`와 `/proc/vmallocinfo`는 root만 읽는 0400이다(SELinux는 shell에게 `proc_slabinfo` 읽기를 허락하지만 DAC가 막는다). user 빌드라 `adb root`가 없다.

**바꾼 것: 재는 쪽.**

1. 커널에 `CONFIG_SLUB_DEBUG=y`(검사는 켜지 않는다, `CONFIG_SLUB_DEBUG_ON` 꺼짐). 비용은 캐시마다 카운터 몇 개다.
2. `dsa-channeld --memory-report`. init이 root로, 읽기만 하는 자기 도메인(`dsa_memory_report`, `channeld.cil`, 앱의 `/proc` 항목을 읽으려고 `mlstrustedsubject`)과 `readproc` 그룹으로 띄운다. 개발자 모드가 켜져 있을 때 shell이 `debug.dsa.memory_report`를 1로 두면 돈다. 커널 로그에 `memory report:` 줄로 쓴다: `/proc/meminfo`의 커널 항목, slab 캐시 상위 40개와 쓰지 않는 몫, vmalloc을 부른 함수별 합(주소는 쓰지 않는다), 커널 스레드 종류별과 프로세스별 스레드 수(스레드마다 커널 스택 16 KiB), 페이지 테이블 상위 15개, 살아 있는 cgroup과 지워졌지만 메모리를 쥔 cgroup의 수, dm-bufio 캐시(dm-verity 해시 블록) 크기.
3. CI의 메모리 측정(PR #625의 boot check)이 읽기 직후 보고를 받아 `memory.md`와 `memory.json`에 "The guest kernel's memory" 표를 더한다. system_server도 함께 적는다: `dumpsys meminfo -d <pid>`의 App Summary와 binder 객체 수, 스레드 이름(숫자를 `#`으로 묶은 모양별 수), `dumpsys -l`의 서비스 목록, `pm list features`, ART 힙 설정, `am dumpheap`의 결과. 원본 파일은 artifact `memory-measurement`의 `memory-kernel*.txt`, `memory-system-server.txt`, `memory-services.txt`, `memory-features.txt`다.
4. 사용자 PC에서 같은 보고를 보려면(개발자 모드): `adb shell "setprop debug.dsa.memory_report 0; setprop debug.dsa.memory_report 1"` 뒤 `adb logcat -d -b kernel | grep "memory report:"`.

**바꾼 것: 줄이는 쪽.**

| 변경 | 무엇이 주나 | 기대 절감(재지 않음) |
|---|---|---|
| 커널 패치 `ext4-rsv-conversion-read-only.patch`: 읽기 전용 장치를 읽기 전용으로 붙인 ext4는 변환 워크큐를 만들지 않고, 읽기와 쓰기로 다시 붙일 때 만든다. 해제, sync, 쓰기 완료가 워크큐 없는 마운트를 견딘다 | APEX 마운트마다 하나씩이던 rescuer 스레드 75개(task_struct, 커널 스택 16 KiB, 워크큐 구조) | 2-3 MiB, 스레드 75개 |
| Linux와 macOS 호스트도 Windows의 부팅 메모리 설정(`swiotlb=2048`, `kfence.sample_interval=0`, dentry·inode·TCP·UDP 해시 크기)을 준다. 상수는 `dsa-host-core`로 옮겼다 | Linux 게스트의 해시 표(3 GiB에서 약 8 MiB), 메모리가 4 GiB를 넘는 VM의 64 MiB 바운스 버퍼 | Linux 게스트에서 약 8 MiB. Windows는 이미 이 값이다. CI의 수치가 Windows 사용자 게스트의 커널 비용과 같은 조건이 된다 |
| Wi-Fi Direct, Wi-Fi Aware, Wi-Fi RTT, Thread 기능을 거둔다(`unavailable-features.xml`). 게스트의 Wi-Fi는 virt_wifi(호스트 네트워크 위의 station 하나)이고 Thread HAL은 이미 뺐다 | system_server의 WifiP2pService, WifiAwareService, RttService, Thread 서비스(각자 스레드, 상태 기계, 힙) | 2-5 MiB |
| `dalvik.vm.heapmaxfree` 8m에서 4m | 힙이 가장 큰 프로세스(system_server, Play 서비스, 화면의 앱)가 GC 사이에 늘어나는 폭. 이 프로세스들은 배경으로 가지 않아 ART가 줄이지 않는다 | 4-8 MiB. 비용은 이 프로세스들의 GC가 두 배 잦아지는 것(동시 GC, 멈춤 1 ms 안) |
| `vm.vfs_cache_pressure` 100에서 200 | 회수(작업 집합 trim 포함) 때 dentry와 inode를 두 배 쉽게 거둔다. 유휴 회수는 원래 모두 버린다 | 회수 가능한 slab이 앱을 쓰는 동안 더 빨리 돌아온다. 크기는 덤프가 보여 준다 |

Windows 게스트 기준 합은 약 8-16 MiB로 본다. 250 MiB 목표에 견주면 작다. 이 변경의 주된 값은 다음 측정에서 slab 100 MiB와 vmalloc 60 MiB가 무엇인지 처음으로 보인다는 데 있다.

**살펴보고 바꾸지 않은 커널 설정과 그 까닭.**

- `CONFIG_SLAB_MERGE_DEFAULT`. config-wsl이 끈다(보안 강화: 캐시를 섞지 않아 힙 공격을 어렵게 한다). 켜면 같은 모양의 캐시가 합쳐져 캐시마다 남는 부분 slab과 CPU별 slab이 준다. 수 MiB에서 10 MiB 남짓을 볼 수 있지만 보안 강화를 푸는 일이라 소유자가 정한다. 덤프의 `slab-total`의 쓰지 않는 몫이 이 판단의 근거다. 덤프의 캐시 이름도 합쳐진 이름으로 바뀌므로 내역을 먼저 본 뒤에 정하는 것이 맞다.
- `CONFIG_NR_CPUS=512`, `CONFIG_NODES_SHIFT=10`(NUMA 노드 1024). per-CPU와 노드별 할당은 실제 CPU와 노드 수로 잡히고, 정적 배열과 `cpumask`, `nodemask`만 커진다. 합쳐 1 MiB 안으로 본다. 큰 호스트에서 VM에 프로세서를 많이 주는 경우를 막을 위험이 더 크다.
- `CONFIG_AUDIT`. 태스크마다 audit 문맥(약 1 KiB, 1347개면 약 1.3 MiB)을 잡지만 SELinux 거절이 logd의 auditd로 가는 길이다.
- BTF는 netbpfload가 쓰므로 남긴다(위 기준선과 같다). ftrace, debugfs도 perfetto와 atrace 때문에 남긴다.
- dm-verity의 kverityd와 kdmflush 스레드. dm 테이블은 fs_mgr와 apexd가 만들므로 DSA가 정하지 않는다. dm-bufio 캐시 크기(`dm_bufio.max_cache_size_bytes`, `max_age_seconds`)는 덤프의 `bufio` 줄을 본 뒤 정한다. 기준선의 "이름 없는 것 64 MiB"가 이 캐시일 수 있다(dm-bufio는 4 KiB 블록을 slab이 아니라 페이지로 잡는다).
- netd의 BPF 맵. 미리 할당되는 해시 맵은 vmalloc에 잡힌다(덤프에 `bpf_map_area_alloc`로 보일 것이다). 크기는 netd와 Connectivity 모듈이 정해서 이미지 설정으로 바꿀 수 없다.
- binder 버퍼. 프로세스마다 1 MiB 남짓을 가상으로 매핑하고 쓰는 페이지만 잡으므로 바꿀 것이 없다. binder 객체의 slab은 덤프가 보여 준다.
- 공유 워크큐 하나로 ext4 변환을 모으는 안. loop 위의 쓰기 가능한 ext4가 아래 ext4의 변환을 기다리는 경우 한 줄로 선 작업이 서로를 기다릴 수 있어 택하지 않았다. 읽기 전용 장치일 때만 만들지 않는 쪽은 그런 의존이 없다.

**system_server: 설정과 오버레이로 할 수 있는 것.** Android 16과 17의 `SystemServer.startOtherServices`는 서비스 대부분을 기능(`hasSystemFeature`)으로 가르고, `config_` 자원으로 가르는 것은 적다. 패키지 이름 문자열로 켜지는 서비스(content capture, app prediction, smartspace, search UI, translation, music recognition, system captions, ambient context, rotation resolver, wallpaper effects)는 AOSP의 `config.xml`이 비워 두므로 이미 시작하지 않는다(Pixel의 GMS 오버레이가 채우는 값이다). 그래서 기능을 거두는 쪽이 이번에 할 수 있는 거의 전부였다. 기능이 남은 것 가운데 일부러 남긴 것: 인쇄(`android.software.print`, 앱의 인쇄를 호스트 프린터로 잇는 길), 백업, 동반 기기(CDM이 없으면 `getSystemService`가 null이라 죽는 앱이 있다), MIDI(앱 사이의 가상 MIDI), 배경 화면 서비스(`config_enableWallpaperService`를 끄면 `WallpaperManager`를 부르는 앱이 `DeadSystemException`으로 죽는다), 자동 완성과 자격 증명(비밀번호 관리자와 패스키). 어느 것을 끄면 얼마가 주는지는 `dumpsys meminfo`로 나뉘지 않으므로, 하나씩 끈 이미지로 재야 한다.

**system_server: 프레임워크 빌드가 있어야 하는 것.** Framework 빌드(`framework.yml`)는 이미지의 빌드와 같은 소스에서 라이브러리 몇 개만 만들고 `services.jar`를 만들지 않는다. 다음은 모두 `frameworks/base`의 `services.jar`를 고쳐야 한다:

- 서비스를 시작하지 않거나 늦게 시작하기: 전화 프로세스(`com.android.phone`, 영구 프로세스라 system_server가 바로 다시 띄운다), WebView zygote(`WebViewUpdateService`가 부팅 때 띄운다, PSS 6.9 MiB), 텔레포니 레지스트리, 음성 상호작용. 시작하지 않을 때는 `getSystemService`가 null을 돌려 죽는 앱이 없게 빈 서비스를 남겨야 한다.
- 필요한 빌드: 이미지의 빌드 번호와 같은 `frameworks/base` 체크아웃(Framework 빌드가 이미 받는 소스), `m services`로 `services.jar`, 그리고 system_server의 미리 컴파일한 코드(`/system/framework/oat/x86_64/services.odex`, `.vdex`, `.art`). 미리 컴파일한 것을 jar와 함께 바꾸지 않으면 odrefresh가 첫 부팅에 기기에서 다시 컴파일한다(부팅이 수십 초 길어지고 `/data`에 둔다). 바꾼 jar는 이미지 서명(시스템 파티션의 dm-verity와 vbmeta)에 다시 들어가야 하는데, 이 부분은 지금 make-image가 시스템 파일을 바꿀 때와 같은 길이다. CI 비용은 Framework 빌드에 `services` 대상 하나를 더하는 것(수십 분)이다.
- zygote 프리로드 줄이기. `/system/etc/preloaded-classes` 목록만 고치면 거의 바뀌지 않는다: 프리로드되는 클래스 대부분이 부팅 이미지(`boot.art`)에 이미 들어 있고, 부팅 이미지는 빌드의 부팅 이미지 프로필로 dex2oat가 만든다. 부팅 이미지를 다시 만들려면 프레임워크 빌드가 필요하다. 위 기준선의 까닭(그 페이지를 모든 앱이 나눠 쓴다)도 그대로라 이번에는 하지 않았다.
- system_server의 힙 덤프. user 빌드에서 `am dumpheap`은 디버그 가능하지 않은 프로세스를 거절하고, perfetto의 Java 힙 그래프도 user 빌드에서는 profileable 앱만 받는다. 서비스별 힙을 보려면 `ro.debuggable=1` 시험 이미지가 있어야 한다. CI는 시도하고 거절된 답을 보고에 적는다.

**ART 힙 설정.** `heapmaxfree`만 바꿨다(위 표). `heaptargetutilization` 0.75와 `heapminfree` 512k는 이미 1 GiB 휴대전화의 값이다. `heapgrowthlimit`과 `heapsize`는 늘어날 수 있는 한도일 뿐 잡는 메모리가 아니므로, 앱이 쓸 수 있는 메모리를 줄이지 않게 그대로 둔다. `heapstartsize`는 첫 GC 전까지만 의미가 있어 두었다.

**확인하지 못한 것.**

- 위 모든 절감은 기대값이다. Image test(커널은 이 커밋에서 만든 것)의 메모리 측정이 처음 돌 때 덤프와 함께 잰다.
- 덤프가 실제로 나오는지: SELinux enforcing에서 `dsa_memory_report`가 `/proc/<pid>`, `/sys/fs/cgroup/cgroup.stat`, `/sys/module/dm_bufio/parameters/current_allocated_bytes`를 읽을 수 있는지, init이 `debug.` 속성 변경으로 서비스를 띄우는지, `logcat -b kernel`이 `/dev/kmsg`에 쓴 줄을 보여 주는지. 안 나오면 측정은 실패하지 않고 job summary에 경고와 까닭을 적고, `memory-kernel-log.txt`에 커널 로그를 남긴다.
- ext4 패치가 커널 빌드에 붙고 APEX 마운트에서 스레드가 사라지는지: 덤프의 `threads kernel ext4-rsv-conver` 줄이 없어져야 한다. `/data`가 ext4면 그 마운트의 하나는 남는다.
- 거둔 Wi-Fi 기능을 찾는 앱이 그 하드웨어가 없는 기기처럼 동작하는지.

## R27. 호스트 주소 변환: 행정 경계 데이터 고르기 (FR-36)

FR-36의 주소 변환(`Geocoder`)을 호스트가 무엇으로 답할지 2026-11-10에 조사했다. 처음에는 공개 서버(Nominatim, `CLGeocoder`)를 썼으나, 소유자가 같은 날 FR-36을 바꿨다(PR #579). Google Play가 없을 때만 DSA가 답하고, 호스트에 둔 행정 경계 데이터로 좌표를 시, 구, 동 수준의 주소로 바꾸며, 서버와 키 없이 위치가 밖으로 나가지 않아야 한다. 아래는 그 조건으로 다시 고른 결과다.

### 서버 쪽 백엔드를 쓰지 않는 까닭

- Windows `Windows.Services.Maps.MapLocationFinder`: Microsoft 문서가 지도 인증 키(`MapService.ServiceToken`, Bing Maps 키)를 요구하고, 이 네임스페이스를 사용 중단으로 표시하며, Bing Maps for Enterprise가 퇴역하면 데이터를 더 받지 못한다고 적는다.
- OpenStreetMap Nominatim(공개 서버): 키는 없지만 사용 정책이 초당 1회, 앱 식별 User-Agent, 출처 표시를 요구하고, 많은 설치본의 기본값으로 쓰는 것을 막는다. 위치가 OSMF 서버로 간다.
- macOS `CLGeocoder`: 위치를 Apple 서버로 보낸다. FR-36의 "위치가 밖으로 나가지 않는다"에 맞지 않아 macOS도 같은 데이터를 쓴다.
- Linux: GeoClue는 위치만 주고 주소 변환은 없다.

### 데이터 후보

| 데이터 | 범위 | 라이선스 | 판단 |
|---|---|---|---|
| vuski/admdongkor `ver20261001` (통계청 SGIS 행정동 경계 가공본) | 한국 행정동 3,559개, 시도/시군구/읍면동 이름 | 데이터는 CC BY 4.0, 원자료 출처 표시(공공누리 제1유형) 의무 유지 | 채택. 최신이고 WGS84 GeoJSON이며 `adm_nm`에 시도부터 동까지 한국어 이름이 다 있다 |
| geoBoundaries gbOpen KOR ADM3 | 한국 읍면동 3,504개 | 공공누리 제1유형(KoStat 원자료) | 보류. 같은 원자료이고 대표 연도가 더 오래되었다 |
| geoBoundaries KOR ADM2 | 시군구 229개 | CC BY 3.0(citypopulation.de) | 쓰지 않음. 행정동에 시군구가 이미 들어 있다 |
| 행정안전부 도로명주소 구역 도형, 브이월드 | 더 정밀한 행정동 경계 | 공공누리 | 쓰지 않음. 신청이나 로그인이 필요해 스크립트로 고정해 받을 수 없다 |
| Natural Earth 5.1.2 admin-1, admin-0, populated places (1:10m) | 세계 시도급 4,596개, 나라 258개, 도시 7,342개, 이름 26개 언어 | 퍼블릭 도메인 | 채택. 세계의 나라, 시도, 도시 |
| OSM 파생 데이터(ODbL) | 세계 상세 경계 | ODbL(출처 표시, 파생 데이터베이스의 동일 조건 공유) | 쓰지 않음. 위 두 데이터로 요구 범위를 채우고, 파생 데이터베이스를 배포물에 넣을 때의 조건이 더 무겁다 |
| GeoNames cities | 세계 도시 | CC BY 4.0 | 쓰지 않음. Natural Earth populated places가 퍼블릭 도메인이고 번역 이름이 있다 |

세계의 "시" 수준은 경계가 아니라 도시 점으로 채운다. Natural Earth에는 이름 있는 도시 경계가 없고, geoBoundaries의 세계 ADM2(CGAZ)는 나라마다 라이선스가 섞여 있고 크기가 수백 MB다. 그래서 세계에서는 시도 경계 안의 좌표에 같은 나라에서 가장 가까운 도시(50 km 이내)를 시로 붙인다. 한국은 행정동 경계가 시, 구, 동을 모두 준다.

### 방법

- `dsa/host/core/geodata/build-geodata.py`(표준 라이브러리만 쓰는 스크립트)가 네 GeoJSON을 커밋 해시로 고정한 주소에서 받아 SHA-256을 확인하고(`.scratch/geodata/`에 캐시), 경계를 Douglas-Peucker로 줄인 뒤 한 파일 `address-boundaries.bin`(형식은 스크립트 머리말)과 출처 표시 `NOTICE.txt`로 쓴다. 좌표는 1e-5도 단위 정수의 차분을 가변 길이로 담는다. 허용 오차는 한국 행정동 15 단위(약 15 m), 세계 시도 1000 단위(약 1 km)다.
- 한국 행정동 이름은 `adm_nm`("경기도 수원시장안구 파장동")에서 나눈다. 시도는 `admin_area`, 시는 `locality`(특별시, 광역시, 특별자치시는 그 자체, 도에서는 시나 군), 구는 `sub_locality`(광역시의 군 포함), 읍면동은 `thoroughfare`와 `feature_name`이다. 한 낱말로 붙은 "수원시장안구"는 "수원시"와 "장안구"로 나눈다.
- 빌드 뒤 스크립트가 알려진 좌표(서울시청, 수원 장안구, 파리, 맨해튼, 도쿄역)를 다시 읽어 시도와 시, 구를 확인하고, 틀리면 실패한다.
- 설치본은 이 파일을 호스트 프로그램 옆에 둔다. Windows는 설치 폴더의 `geodata\`, Linux는 `/usr/lib/dsa/geodata/`, macOS는 `DSA.app/Contents/Resources/geodata/`. `build.ps1`, `build-packages.sh`, `build-app.sh`가 스크립트를 부르거나 미리 만든 폴더를 받는다.
- 호스트(`dsa_host_core::boundaries`, `geocode::BoundaryGeocoder`)는 첫 역방향 조회 때 파일을 읽어 층마다 격자(전형적인 영역 크기, 0.01도에서 2도)를 만들고, 칸의 후보 중 상자에 드는 영역만 홀짝 규칙으로 점-다각형 판정을 한다. 행정동 층에서 찾지 못하면 시도 층, 어디에도 없으면 15 km 안의 가장 가까운 도시(해안선 단순화로 바다 쪽에 남은 해안 좌표)를 쓴다. 정방향 조회(주소를 좌표로)는 파일을 읽지 않고 빈 결과로 답한다. 10분 동안 조회가 없으면 메모리에서 내린다.
- 언어는 요청의 BCP 47 태그로 고르고(번체 중국어는 `zht`), 없으면 영어, 그다음 자체 이름이다. 한국 행정동은 한국어 이름만 있으므로 나라 이름만 언어를 따른다. 주소 한 줄은 한국어, 일본어, 중국어에서 나라부터 공백으로, 다른 언어에서 작은 단위부터 쉼표로 잇는다.

### 결과

| 항목 | 값 |
|---|---|
| 원본 GeoJSON 합계 | 108,025,126 바이트(한국 34,652,038, admin-1 40,726,851, admin-0 13,287,234, 도시 19,359,003) |
| 경계 점 수 | 2,032,871 → 658,897(한국 285,435, 세계 373,462) |
| 영역 | 한국 행정동 3,559, 세계 시도 4,523(단순화 뒤 남은 것), 도시 7,342 |
| 이름 | 15,452개, 문자열 174,039개 |
| `address-boundaries.bin` | 6,866,741 바이트, SHA-256 `56db00caf885de4db24e7574e42bc53f6c3c78603227eea6e1013d7faf316dd9` |
| 빌드 시간 | 약 20초(이 PC, Python 3.13, 캐시된 원본) |

서울시청 좌표(37.5663, 126.9779)는 "서울특별시 중구 명동"으로 나온다. 행정동 경계라서 법정동(소공동)이 아니라 행정동(명동)이다.

### 확인하지 못한 것

- Rust 쪽 조회 시간과 메모리는 실제 파일로 재지 않았다. 이 PC에서는 빌드와 시험을 하지 않는다. 단위 시험은 3,600개 행정동 크기의 합성 데이터에서 조회 한 번이 5 ms 아래인지(디버그 빌드)만 본다. 파일을 읽은 동안의 메모리는 점 약 5.3 MB(8바이트 × 658,897)와 문자열 약 3 MB로 어림한다.
- 세 호스트의 설치본에서 실제 앱이 주소를 받는지 확인하지 않았다(FR-36의 수동 확인).
- Google apps 이미지에서 `ro.boot.dsa.gapps=on`일 때 정적 오버레이의 `requiredSystemPropertyName`이 패키지 스캔에서 적용되어 Google의 주소 변환이 묶이는지, 실제 이미지로 확인하지 않았다.
- 단순화로 경계 근처(약 15 m, 세계는 약 1 km) 좌표가 이웃 영역으로 갈 수 있다. 행정동 경계 사이의 틈에 든 좌표는 시도와 가까운 도시로만 답한다.
- 행정동 데이터는 해마다 바뀐다. 갱신은 스크립트의 고정 주소와 해시를 바꾸는 변경으로 한다.

## R28. 게스트 KeyMint가 호스트의 루트 키를 쓰게 하는 길 (FR-36)

### 확인한 자료

- 두 빌드의 매니페스트(`manifest_15581820.xml`, `manifest_16373615.xml`, SHA-256이 잠금과 같다)와 거기서 고정한 커밋의 소스. 프로젝트마다 그 커밋 하나를 받았다(`git fetch --depth=1`, `.scratch/fr36-aosp/<android>/`). `hardware/interfaces`(16 `aa229558`, 17 `0162af69`), `system/keymint`(16 `748182e1`, 17 `cfeefc94`), `device/google/cuttlefish`(16 `4d368849`, 17 `283645aa`), `system/sepolicy`(16 `43e4494a`, 17 `e066568e`), `system/apex`(16 `97548ed5`, 17 `ce2bb9f7`), `system/core`(16 `e9d1fa37`의 `init`), `external/rust/crates/openssl`(16 `01a619f8`, 17 `4dcd5205`), `external/boringssl`, `frameworks/native`의 `libs/binder/rust`, `system/librustutils`, `external/rust/android-crates-io`의 상자 폴더.
- 16 x86_64 대상 파일(`aosp_cf_x86_64_only_phone-target_files-15581820.zip`)의 `VENDOR/apex/com.android.hardware.keymint.rust_nonsecure.apex`(풀어서 `apex_payload.img`의 파일과 각 ELF의 `DT_NEEDED`), `VENDOR/etc/selinux/vendor_file_contexts`, `META/vendor_filesystem_config.txt`, `VENDOR/lib64`.

### 찾은 것

- 이미지의 KeyMint는 `hardware/interfaces/security/keymint/aidl/default`의 `apex` 모듈 `com.android.hardware.keymint.rust_nonsecure`다. 바이너리는 `android.hardware.security.keymint-service.nonsecure`(`rust_binary`, `main.rs`), TA는 `libkmr_ta_nonsecure`(`ta/`)다. APEX의 rc는 `service vendor.keymint-default ...`, `class early_hal`, `user nobody`이고 `group` 줄이 없다. init은 `group`이 없으면 gid를 바꾸지 않으므로(`system/core` `init/service_utils.cpp`의 `if (attr.gid)`) 위 서비스는 gid 0으로 돈다.
- 루트 KEK는 `ta/soft.rs`의 `Keys::root_kek`가 주는 16바이트 0이고, `kek_context`는 기본값(빈 값)이다. 키 덩어리는 그 맥락을 `kek_context`로 적고(`system/keymint` `common/src/keyblob.rs`), 열 때 그 맥락으로 `root_kek`를 다시 부른다(`ta/src/lib.rs`의 `keyblob_parse_decrypt_backlevel`).
- 갱신이 필요하다는 판단은 `ta/src/lib.rs`의 `keyblob_parse_decrypt`가 한다. 16은 OS 판과 패치 수준만 본다. 17은 `RetrieveKeyMaterial::kek_context_is_outdated`(기본 `false`)를 더해, 그것이 참이면 `KEY_REQUIRES_UPGRADE`를 답하고 `upgrade_key`(`ta/src/keys.rs`)가 새 맥락과 루트 키로 다시 싼다. 16에는 이 갈고리가 없다.
- APEX의 `file_contexts`는 바이너리를 `hal_keymint_default_exec`으로 붙인다. `system/sepolicy`의 `vendor/hal_keymint_default.te`가 `hal_server_domain(hal_keymint_default, hal_keymint)`이므로 그 영역은 `hal_keymint_server`에 든다. Cuttlefish의 `vendor_file_contexts`는 `/vendor/bin/hw/android\.hardware\.security\.keymint-service\.nonsecure`를 `hal_keymint_rust_exec`으로 붙이므로, 같은 이름으로 vendor에 두면 다른 영역이 된다.
- APEX의 VINTF 조각은 셋이다. `android.hardware.security.keymint-service.xml`(IKeyMintDevice 4/17은 5, IRemotelyProvisionedComponent 3), `secureclock-service.xml`, `sharedsecret-service.xml`. `kmr_hal`의 `ALL_HALS`가 이 넷을 등록한다. APEX는 `etc/permissions/android.hardware.hardware_keystore.xml`도 가진다.
- APEX의 바이너리는 Rust 상자를 공유 라이브러리(`*.dylib.so`, APEX의 `lib64`)로 링크한다. 그 밖에 `libcrypto`, `libssl`, `libcutils`, `libbase`, `libc++`, `liblog`, `libbinder_ndk`가 필요하다. 16의 vendor에는 `libssl.so`만 없다. 17은 업스트림 바이너리가 이미 `prefer_rlib: true`다.
- apexd는 선택 속성 값이 `none`이면 그 모듈의 APEX를 하나도 쓰지 않는다(`apexd/apex_file_repository.cpp`의 `select_filename == "none"`, 16과 17 같다).

### 결정

- `hardware/interfaces` 패치(16, 17 같은 내용): `ta/host_key.rs`의 `HostKeys`가 `/dev/dsa_keymint/root_key`를 처음 루트 키가 필요할 때 읽고(서비스 시작부터 최대 30 s 기다린다), 프로세스가 끝날 때까지 둔다. 키가 있으면 새 덩어리는 그 키와 맥락 `dsa-host-root-key-v1`을 쓰고, 빈 맥락의 덩어리는 0 키로 열되 오래된 것으로 답한다. 키가 없으면 업스트림과 같다. `Android.bp`에 Rust 상자를 정적으로 링크한 `dsa-keymint` 바이너리를 더하고, 트리에 키와 인증서가 없는 APEX 모듈을 뺀다.
- `system/keymint` 패치(16만): 17의 `kek_context_is_outdated` 코드를 그대로 가져온다.
- 이미지: `/vendor/bin/hw/dsa-keymint`를 `hal_keymint_default_exec`으로 붙여 APEX와 같은 영역에서, 같은 서비스 이름으로 돌린다(`user nobody`, `group nobody`). `libssl.so`, VINTF 조각 셋, 기능 파일을 함께 두고, `androidboot.vendor.apex.com.android.hardware.keymint=none`으로 APEX를 쓰지 않는다. APEX 파일은 이미지에 남는다.

### 확인하지 못한 것

- 빌드 전부. 축소 트리에서 vendor Rust 바이너리를 빌드한 적이 없다. 잠금에 더한 프로젝트와 상자는 Android.bp의 의존을 정적으로 따라가 고른 것이고, AIDL Rust 백엔드, vendor 변형의 LLNDK 스텁, `TARGET_COPY_OUT_VENDOR := vendor`의 설치 경로, 축소 트리의 clippy가 통과하는지는 Framework build가 처음 보인다.
- 17 대상 파일의 vendor에 `libssl.so`가 없는지와 그 APEX의 내용. 16 것만 풀어 보았다. `make-image.py`가 바이너리의 `DT_NEEDED`를 vendor와 LLNDK에 대조하므로 빠진 것은 이미지 빌드가 멈춘다.
- 실기 동작: 기존 설치의 키가 모두 갱신되는지, vold의 메타데이터 키 갱신, 키가 없는 부팅에서 호스트 키로 싼 덩어리가 실패하는 모습.

## R29. v0.2.1부터 창 크기 변경이 느려진 까닭: 펜스마다 매핑된 메모리 전체를 다시 읽었다 (FR-4, PR-3)

**기준.** 소유자 PC(Windows 11 Pro 26300, x64, Intel Arc, 2880x2880 프리폼 디스플레이)에서 v0.1.9부터 v0.2.2까지 남은 로그를 읽었다. 호스트: `%LOCALAPPDATA%\DSA\logs\dsa-app-window.log`(`stage=app.summary`와 창을 닫을 때의 요약), `dsa-wm.log`(`stage=wm.summary`), `C:\ProgramData\DSA\logs\dsa-hostd.log`(`stage=service.forwarded`). 게스트(Android 17, v0.2.2, 부팅 뒤 약 80분, Google Play 창 하나): adb로 읽기만 했다(`logcat -d -b all`, `dumpsys gfxinfo com.android.vending [framestats]`, `getprop`). 이 조사에서 빌드하거나 실행하지 않았고, 설치된 DSA와 바탕 화면은 건드리지 않았다. 로그 시각은 UTC이고 설치 로그 파일의 시각은 KST다. 그래서 v0.2.0은 11-09 10:16 UTC, v0.2.1은 11-09 19:26 UTC, v0.2.2는 11-11 00:35 UTC에 설치됐다.

### 로그로 본 크기 변경의 기다림

| 시점 | 창을 닫을 때의 요약(크기 변경 수, 새 크기의 프레임까지 중앙값, 95번째, 최댓값) |
|---|---|
| 11-08 12:21 (v0.1.9) | 6번, 80 ms, 344 ms, 344 ms. `waited=` 54-344 ms |
| 11-09 19:38-21:30 (v0.2.1) | 1-2번씩, 949-3172 ms |
| 11-10 01:32-02:12 (v0.2.1) | 784-1612 ms, 최대 2416 ms |
| 11-11 00:42 (v0.2.2) | 2284 ms(최대 7550 ms), 795 ms(최대 1126 ms) |

v0.2.0이 설치돼 있던 동안(11-09 10:16-19:26 UTC)에는 크기 변경 기록이 없다. 느려진 것은 모두 v0.2.1 뒤다.

### 한 번의 크기 변경을 단계로 나눈 것

task 19(Google Play), 1079x1563, 2026-11-11 01:57:18 UTC. 호스트와 게스트 줄은 `utc`로 맞췄다.

| 단계 | 근거 | 걸린 시간 |
|---|---|---|
| 호스트가 새 크기를 묻고 서비스가 넘김 | `service.forwarded took=0.1` | 0.1 ms |
| 게스트 태스크 크기 변경(에이전트가 적용하고 경계가 바뀜) | `agent.summary applied=4.3 bounds=23.9` | 24 ms |
| 앱의 다시 배치와 다시 그리기 | `wm_relaunch_resume_activity`, `finishDrawing of relaunch ... 303ms`. 다시 시작하지 않은 변경(01:57:09-11)은 `WindowInsets changed`에서 `reportDrawFinished`까지 170-250 ms | 170-416 ms |
| hwcomposer가 새 크기 프레임을 기다림 | `hwc.summary since_bounds=504.9 held=493.3 late=false` | 493 ms(앱이 그 크기로 그린 프레임을 기다린 시간. `late=false`라 500 ms 정착 시한으로 보낸 것이 아니다) |
| hwcomposer 버퍼 다시 만들기, 렌더러 ColorBuffer | `task 19's buffers are now ...` 줄은 같은 밀리초 안 | 1 ms 미만 |
| 창 관리자 텍스처, 앱 창 표시 | `wm.summary create=0.3`, `app.summary resize_buffers=0.0 present=0.1` | 1 ms 미만 |
| 전체 | `app.summary waited=717.6` | 718 ms |

v0.1.9의 같은 줄(11-08 12:20)은 `waited=53.8-80.3`이다. 커진 단계는 앱이 새 크기로 그리는 시간 하나다. 호스트 쪽과 hwcomposer, 렌더러의 버퍼, 창 표시는 모두 1 ms 안팎으로 그대로다. 드래그 동안 호스트는 앞의 요청에 프레임이 오기 전에는 다음 크기를 덜 보내므로, 한 단계가 느리면 다음 크기의 기다림에 그만큼이 더해진다(`a task window keeps its size: no ... frame came in 2009-2197 ms`가 여러 번 있다).

### 앱의 프레임이 느린 까닭

`dumpsys gfxinfo com.android.vending`: 프레임 13380개 가운데 13367개가 늦었다. 중앙값 150 ms, 90번째 200 ms, GPU 시간은 모두 1 ms 이하다. "Slow issue draw commands" 13351개, "Slow UI thread" 7517개. `framestats`로 보면 프레임마다 `IssueDrawCommandsStart`에서 `SwapBuffers`까지 82-100 ms이고, `DequeueBufferDuration`은 3-8 us, `QueueBufferDuration`은 0.16-0.54 ms다. 곧 RenderThread가 GL 명령을 ANGLE과 게스트 Vulkan 드라이버로 내보내는 동안 기다린다. 크기를 바꾸면 앱은 두세 프레임(동기화로 취소된 한 번, 다시 그린 한 번, 다시 시작하는 앱이면 그 첫 프레임)을 그려야 새 크기 프레임을 내므로 300-500 ms가 된다.

그 동안 드라이버가 하는 일은 guest/mesa 0005(`94262816`, v0.2.0과 v0.2.1 사이)다. 0005는 게스트가 사본을 두는 호스트 가시 메모리를 coherent로 알리고, 앱이 펜스가 신호된 것을 처음 볼 때마다(`vkGetFenceStatus`, 동기화 fd), 그리고 `vkWaitSemaphores`, `vkDeviceWaitIdle` 뒤마다 그 장치의 모든 사본을 `VK_WHOLE_SIZE`로 다시 읽는다(R26의 1). ANGLE은 제출마다 펜스를 확인하므로 프레임마다 한두 번, 앱이 매핑한 메모리 전체가 Hyper-V 소켓을 건너온 뒤 다시 해시된다. guest/mesa 0007(`d9194885`, v0.2.2)은 매핑한 할당만으로 줄였지만, Skia와 ANGLE은 유니폼, 정점, 스테이징 버퍼를 계속 매핑해 두고(수 MiB에서 수십 MiB), v0.2.2에서도 중앙값 795-2284 ms로 나아지지 않았다. 다른 후보는 시기나 단계가 맞지 않는다. 주 모니터 밀도로 시작(`9b7c92e2`)과 96 x 배율(PR #560)은 버퍼 크기와 재시작에 관한 것이고 프레임마다의 시간을 바꾸지 않는다. CPU 업로드(PR #591), 캡션 밝기 표본(PR #555), 장치 복구 확인과 기다림 상한(PR #550)은 호스트 쪽이거나 v0.2.2에만 있는데, 호스트 쪽 단계는 1 ms 안이고 느려짐은 v0.2.1부터다. hwcomposer의 프레임 붙잡기(`b883d6f9`)는 v0.1부터 있고, 위의 `held`는 앱이 그리는 시간만큼이다.

### 고친 것

읽어 오기가 앱이 매핑한 양이 아니라 장치가 쓴 양만큼 들게 했다. 전송 형식은 그대로다.

- 호스트(patches/0020, `dsa_shadow_record.h`): 렌더러가 할당마다 4 KiB 블록별로 게스트 사본이 가진 내용의 해시를 둔다. 플러시로 받은 바이트와 읽어 오기로 보낸 바이트가 그 해시가 된다. 읽어 오기 요청의 범위마다 블록 하나라도 바뀌었거나 모르는 것이 있으면 범위 전체를, 아니면 크기 0을 보낸다. 해시는 보내기 전에 잡으므로 그 사이 장치가 쓴 블록은 다음에 다시 간다. 해제된 할당은 잊는다. 메모리는 4 KiB마다 16바이트다.
- 게스트(guest/mesa 0010, `dsa_shadow_blocks.h`의 `receive`): 할당을 64 KiB 조각으로 나눠 묻는다. 받은 조각은 블록마다, 받은 내용이 이 사본이 마지막으로 주고받은 것과 다를 때만(장치가 바꾼 블록) 받아들인다. 그 밖의 블록은 앱이 마지막 제출 뒤에 쓴 것을 그대로 두고 다음 제출에 보낸다. 예전에는 읽어 오기 뒤 사본 전체를 호스트 것으로 표시해서, 그 사이 앱이 쓴 것이 덮이고 다시 보내지지도 않았다. 일부만 받은 블록은 다음 플러시에 통째로 간다.
- 섞인 버전: 0020이 없는 렌더러는 모든 조각을 보내고 게스트는 그것을 맞게 받는다(예전 비용). 0010이 없는 게스트는 할당 전체를 묻고, 0020 렌더러는 바뀐 것이 없으면 0을 보낸다. 어느 쪽도 스트림이 어긋나지 않는다.
- 추적: 입력 추적 부팅(`androidboot.hardware.dsa_input_trace=1`)에서 읽어 오기 120번마다 `input-trace stage=driver.readback reads= asked_mb_per_read= received_kb_per_read= taken_kb_per_read= ms_per_read=` 줄을 남긴다. `stage=driver.flush`와 짝이다.

FR-4의 프레임 규칙(창은 앱이 그 크기로 그린 프레임과 함께만 바뀌고, 놓은 크기에 정확히 닿는다)은 hwcomposer와 호스트의 것이고 바꾸지 않았다.

### 회귀를 막는 시험

`dsa/graphics/tests/readback_latency_test.cpp`(Graphics test의 "Test the guest transport on the build machine"): 게스트와 호스트의 절반을 드라이버와 디코더가 하는 대로, 바이트를 세는 메모리 안 소켓으로 돌린다. 앱은 2 MiB 할당 8개(16 MiB)를 매핑하고, 프레임마다 32 KiB씩 네 번 쓰며, 펜스가 있는 제출을 두 번 한다. 비용은 센 양으로 모형화한다: 소켓 1 GiB/s, 해시 4 GiB/s. 기계에 따라 흔들리지 않게 하기 위해서다. 크기 변경 하나를 두 프레임으로 치고, 전송이 더하는 몫이 50 ms를 넘으면 실패한다. 예전의 통째 읽어 오기가 같은 모형에서 50 ms를 넘는지도 확인해서, 시험이 그 회귀를 잡을 수 있음을 보인다. 계산상 지금은 약 32 ms(거의 해시), 예전은 약 94 ms(그 가운데 소켓 약 63 ms)다. 실제 그 기계에서 잰 시간도 함께 찍는다. 이 밖에 `fr4_guest_and_host_agree_after_each_fence`, `fr4_an_app_write_before_the_fence_is_not_lost`, `fr4_a_frame_reads_back_only_what_the_device_wrote`, `shadow_record_test.cpp`, `shadow_blocks_test.cpp`의 `receive` 시험이 있다.

### 기대하는 값

프레임마다의 읽어 오기가 매핑한 메모리 전체(수 MiB에서 수십 MiB)에서 장치가 쓴 조각(대개 0)과 조각마다 8바이트로 준다. 남는 드라이버 비용은 v0.1에도 있던 제출마다의 플러시 해시와 렌더러의 해시다. 그래서 앱의 프레임은 v0.1 수준(수 ms에서 20 ms 안팎)으로, 크기 변경은 v0.1의 80 ms 안팎(에이전트 약 25 ms, 앱이 두세 프레임, 표시 1 ms)으로 돌아올 것으로 본다. Google Play처럼 크기 구간을 넘을 때 액티비티를 다시 만드는 앱은 그 다시 만들기 시간(로그로 161-416 ms, 지금의 느린 프레임 포함)이 더해진다. 그것은 앱의 동작이다.

### 확인하지 못한 것

- 고친 뒤의 실제 값 전부. 이 PC에서 빌드하거나 실행하지 않았다. 패치가 고정 리비전에 붙는지는 그 파일들의 사본에서 확인했고(guest/mesa 0001-0010, 호스트 0006, 0011, 0020), 빌드와 C++ 시험은 Graphics test가 처음 돌린다. PC에서는 같은 조건(Google Play 창 하나, 크기 변경 추적 켬)으로 `app.summary waited=`의 중앙값과 95번째, `hwc.summary held=`, `dumpsys gfxinfo`의 중앙값과 "Slow issue draw commands"를 앞뒤로 잰다. 입력 추적 부팅이면 `driver.readback`의 `received_kb_per_read`가 0 가까이인지 본다.
- 실제로 프레임마다 몇 MiB를 다시 읽었는지. 0005 뒤의 읽어 오기에는 추적 줄이 없었다. 이제 위의 줄이 남긴다.
- 렌더러의 해시 비용. 조각마다 호스트가 매핑을 읽어 해시하므로, 매핑이 아주 큰 앱(수백 MiB)에서는 펜스마다 수십 ms가 될 수 있다. 그때는 제출이 없었던 장치의 조각을 건너뛰는 것이 다음 단계다.
- 다른 GPU. 읽어 오는 형식은 HOST_CACHED뿐이므로 렌더러는 캐시되는 메모리만 읽는다. 다른 메모리 형식 구성을 가진 GPU에서는 재 보지 않았다.

## R30. 앱의 툴바가 창 제목 표시줄 아래에 그려진 까닭: 프리폼 창은 상태 표시줄 인셋을 받지 못한다 (FR-22)

**기준.** 소유자 PC(Windows 11, 200% 모니터, v0.2.2, Android 17 `16373615`)에서 FLO(`skplanet.musicmate`, MainActivity 플레이어 화면)의 메뉴와 캐스트 아이콘이 캡션의 앱 아이콘과 이름에 겹치고, 접기 버튼이 닫기 버튼 아래에 놓였다(`.scratch/widget/flo-app.png`). 폰에서는 바르게 보인다. adb로 읽기만 했다(`dumpsys window`, `dumpsys activity top`, `getprop`). 프레임워크 소스는 이미지가 만들어진 커밋(Android 17 `94b4c163`, Android 16 `45034f06`)의 것을 읽었고, Android 16 `services.jar`의 dex는 `.scratch/aosp-inputs`의 target_files에서 꺼내 읽었다.

### 본 것

- 태스크에는 에이전트가 준 인셋 소스 둘이 있다: `type=statusBars frame=[-1048576,507][1048576,565]`, `type=captionBar` 같은 프레임(58 px). 창은 `EDGE_TO_EDGE_ENFORCED`, `fitTypes`가 비어 있다.
- 그런데 앱 프로세스의 `InsetsController` 상태(`dumpsys activity top`)에는 `captionBar` 소스 하나뿐이다. 상태 표시줄 소스가 앱에 닿지 않는다.
- 원인은 `InsetsPolicy.enforceInsetsPolicyForTarget`이다. 떠 있는 창 모드(프리폼, PIP)이면 `WindowInsets.Type.captionBar()`와 IME만 남기고 나머지 소스를 버린다(Android 16과 17 같음). DSA의 태스크는 모두 프리폼이다. 그래서 `getInsets(statusBars())`로 툴바를 미는 앱(FLO와 대부분의 edge-to-edge 앱)은 0을 받아 툴바를 창 맨 위, 캡션 아래에 그린다. `systemBars()`나 `captionBar()`를 쓰는 앱, 그리고 `fitsSystemWindows`로 맞추는 앱은 캡션 바 인셋이 `systemBars()`에 들어가므로 이미 바르다.
- `mForcedConsumingTypes=statusBars navigationBars`, `mForciblyShowingTypes`는 프리폼 창이 있을 때 `InsetsPolicy.updateSystemBars`가 정하는 값이고, 전역 소스 제공자(시스템 UI의 막대)에만 `FLAG_FORCE_CONSUMING`을 붙인다. 태스크의 로컬 소스는 `flags=`가 비어 있어 강제로 소비되지 않는다. 그래서 레거시 `SYSTEM_UI_FLAG`를 쓰는 앱도 폰과 같이 인셋을 받는다(DecorView의 `consumingStatusBar`는 앱이 `LAYOUT_FULLSCREEN` 없이 맞출 때만).
- 높이: 200%(192 DPI)에서 표준 창의 캡션과 위 테두리는 58 px(`AdjustWindowRectExForDpi`)이고, 인셋도 58 px다. 맞다. 131 x 배율 규칙에서 Android는 262 dpi로 그리고 58 px은 35.4 dp다. 배율에 따라 31 px@130 dpi(38.2 dp)에서 139 px@654 dpi(34.0 dp)까지로, dp가 배율마다 조금 다르다(Windows가 캡션 높이를 정수 픽셀로 반올림한다).
- 상태 표시줄 리소스: `status_bar_height_default`와 `status_bar_height_portrait`는 24 dp이고(`status_bar_height`, `_landscape`는 그것을 가리킨다), 큰 화면용으로 `sw1500dp`/`sw1900dp`와 저밀도가 함께 붙은 구성에서 36 dp, 48 dp다. 리소스를 직접 읽는 앱(`getIdentifier("status_bar_height", ...)`)은 24 dp를 받아 캡션보다 11 dp 짧게 민다.
- 앱이 edge-to-edge가 아니면 DecorView가 상태 표시줄 색 뷰를 `systemBars()` 인셋 높이(캡션 바 포함)로 위에 그리므로, 캡션 아래에는 앱의 상태 표시줄 색이 이미 그려진다. 앱이 색을 정하지 않으면 그 자리에 창 배경이 보인다.

### 고친 것

- 이미지(`make-image.py`, `SERVICES_CODE`): `services.jar`의 `enforceInsetsPolicyForTarget`에서 `isFloating` 다음의 `captionBar()` 호출 하나를 `systemBars()`로 바꾼다. 같은 크기의 명령에서 메서드 번호만 바뀐다. 게스트에는 내비게이션 막대도 시스템 UI도 없으니 시스템 막대 소스는 에이전트가 주는 것뿐이다. 바뀐 jar는 저장된 dex를 4바이트에 맞추고(맞지 않으면 system_server가 20 MiB를 메모리로 복사한다), 공개 빌드의 필터(speed-profile), 클래스 로더 문맥, 앱 이미지, 프로필(`services/art-profile`과 `art-profile-extra`, 빌드의 frameworks/base 커밋, `inputs.json`에 고정)로 다시 컴파일한다. Android 16 jar로 바꾸기를 확인했다(`classes3.dex`, 다시 적용하면 바뀌는 것 없음, 정렬 유지).
- 프레임워크 오버레이: `status_bar_height`, `_default`, `_portrait`, `_landscape`를 배율마다의 밀도(130, 164, 196, 228, 262, 294, 328, 360, 392, 458, 524, 590, 654 dpi)에서 그 배율의 제목 표시줄 높이로, 밀도 없는 값은 가장 큰 값(100%의 38.2 dp)으로 둔다. `getDimensionPixelSize`가 그 밀도에서 제목 표시줄의 픽셀과 같게 반올림한다.
- `check-image.py`는 바꾸기가 없거나, dex가 정렬되지 않았거나, 오버레이의 높이가 제목 표시줄과 다른 이미지를 거부한다.

### 확인하지 못한 것

- 실기. 이미지를 빌드하거나 PC에서 실행하지 않았다. Image test가 처음으로 jar를 바꾸고 다시 컴파일한다. PC에서는 FLO 플레이어 화면의 툴바가 캡션 아래에 오는지, `dumpsys activity top`의 앱 `InsetsController`에 `statusBars` 소스가 있는지, `dumpsys window`의 `services.odex`가 쓰이는지(부팅 로그에 `services.jar` dex2oat나 `odex` 거부가 없는지) 본다.
- Android 17 `services.jar`. Android 17의 소스는 같은 꼴이지만, 그 빌드의 dex는 읽어 보지 않았다. 다른 꼴이면 바꾸기가 이미지 빌드를 멈춘다.
- 오버레이 값이 앱에 닿는지. 큰 화면 구성(`sw1500dp` 이상과 저밀도)에서는 프레임워크의 더 구체적인 값(36, 48 dp)이 이길 수 있다. 그때 앱은 캡션보다 더 아래로 민다(가리지는 않는다). 밀도를 지키는 앱은 가장 가까운 밀도의 값을 받아 1-2 px 다를 수 있다.
- 리눅스와 macOS 호스트. 오버레이의 높이는 Windows 제목 표시줄의 것이다. 다른 호스트의 제목 표시줄 높이가 다르면 리소스를 읽는 앱만 몇 px 어긋난다. 인셋은 어느 호스트에서나 그 호스트가 알린 높이다.

## R31. v0.2.3에서도 창 크기 변경이 느린 까닭: 새 크기가 에이전트 스레드에서 접근성 트리 읽기 뒤에 줄을 섰다 (FR-4, FR-18)

**기준.** 소유자 PC(Windows 11, Intel Arc 내장 GPU, 배율이 다른 모니터 셋, v0.2.3)의 진단 묶음 두 개를 읽었다. 하나는 Google Play 창을 1.5초 간격으로 26번 프로그램으로 크기를 바꾼 것(2026-11-11 23:46:38-23:47:25 UTC, task 3), 다른 하나는 FLO 창을 손으로 끌고 최대화한 것(2026-11-12 00:31-00:34 UTC, task 4)이다. 호스트 `dsa-app-window.log`, `dsa-wm.log`, `dsa-hostd.log`와 게스트 `logcat`을 `utc`로 맞췄다. 크기 변경 추적은 요약 줄만 켜져 있었다. 이 조사에서 빌드하거나 실행하지 않았고 설치된 DSA와 바탕 화면은 건드리지 않았다.

### 한 번의 크기 변경을 단계로 나눈 것

Google Play, 1574x1187, 23:46:41.529 요청(`app.summary waited=1487.9`에서 거꾸로 셈).

| 단계 | 근거 | 시각 | 걸린 시간 |
|---|---|---|---|
| 앱 창이 새 크기를 물음 | `app.summary` 43.017 - 1487.9 ms | 41.529 | |
| 라이브 크기가 게스트 창 관리자에 닿음 | system_server `TransitionChain` 줄(에이전트의 창 컨테이너 트랜잭션마다 하나) | 42.906 | 1377 ms |
| 게스트가 새 크기를 보고하고 호스트가 다시 구성 요청을 보냄 | `service.forwarded live=false` | 42.912 | 6 ms |
| 에이전트가 다시 구성 요청을 받고 경계를 읽음 | `agent.summary applied=none bounds=2.8` | 42.916 | 4 ms |
| 앱이 새 크기로 그리고 hwcomposer가 보냄 | `hwc.summary since_bounds=100.5 held=85.4` | 43.012 | 96 ms |
| 렌더러, 창 관리자 텍스처, 앱 창 표시 | `wm.summary create=0.4`, `app.summary resize_buffers=0.0 present=0.1` | 43.017 | 5 ms |

26번 모두 같은 꼴이다. 게스트 쪽(에이전트가 받은 뒤부터 프레임까지)은 85-140 ms이고(`hwc.summary held=` 69-219 ms), R29의 고침(mesa 0010, 호스트 0020) 뒤로 앱의 프레임은 빨라졌다. 나머지 0.1-1.4초는 모두 요청이 게스트 창 관리자에 닿기 전이다. 요청은 1.5초 간격으로 정확히 나갔는데(`waited`에서 거꾸로 센 요청 시각), `service.forwarded`는 1.8-2.4초 간격이다. 호스트의 요청 경로(앱 창, 창 관리자, 요청 스레드, 서비스)에는 기다리는 곳이 없다: 보고는 바로 보내지고, 크기 조절 페이서는 다시 구성 요청마다 풀리며, 서비스의 `took=`는 0.0-0.1 ms다. FLO의 최대화(00:33:12.455 요청)도 라이브 크기가 게스트에 닿기까지 660 ms, 앱이 2880x2876을 처음 그리는 데 1162 ms(`HWUI: Davey! duration=1110ms`, `Skipped 49 frames`)였다.

### 까닭

에이전트는 호스트의 요청, 태스크 읽기, 접근성 트리 내보내기를 모두 스케줄러 스레드 하나에서 차례로 한다. 두 세션 모두 호스트의 UI Automation 클라이언트가 창을 읽고 있었다(`task 3: a UI Automation client reads the window`, `a host accessibility client watches task 3; the accessibility service is on`). 소유자 PC에는 늘 그런 클라이언트가 있다(세션마다 모든 task에 watch가 걸렸다). 트리 읽기는 창마다, 그리고 캐시에 없는 노드마다 앱으로 가는 바인더 호출이고, 앱은 UI 스레드가 비었을 때 답한다. 크기를 바꾼 직후의 앱은 다시 배치하고(Google Play는 다시 구성 때마다 액티비티를 다시 만든다) 그동안 노드가 최대 2000개인 트리를 읽으면 1초 이상 걸릴 수 있다. 바뀐 화면은 250 ms마다 다시 읽힌다. 그 사이 도착한 ResizeTask는 스케줄러 줄 뒤에서 기다렸다. 같은 기다림이 FLO에서 손으로 끈 크기가 2초 안에 오지 않은 것(`no 1028x1004 frame came in the 2052 ms`)도 설명한다. CI의 Graphics test 크기 변경 시험(R29, `readback_latency_test.cpp`)은 드라이버의 전송만 재고, 이미지 부팅 검사에는 크기를 바꾸는 검사도, 접근성 클라이언트도 없었다. 그래서 CI는 이 경우를 만들지 못했다.

에이전트 스레드가 실제로 무엇에 막혔는지는 로그로 확인하지 못했다(그 스레드는 막힌 동안 아무것도 남기지 않는다). 위의 결론은 시각과 코드에서 끌어낸 것이다. 그래서 이번에 아래의 줄들을 더했다.

### 고친 것

- 에이전트: ResizeTask와 SetTaskDensity는 바인더 스레드에서 따로 줄을 세우고 스케줄러의 맨 앞에서(`postFirst`, `Handler.postAtFrontOfQueue`) 함께 도착한 것끼리 순서대로 처리한 뒤 태스크를 바로 한 번 읽는다. 밀도와 그 크기는 여전히 한 번의 변경으로 간다.
- 에이전트: 접근성 트리의 노드는 `dsa-accessibility` 스레드에서 읽고, ID를 매기고 바뀐 것을 보내는 일만 스케줄러 스레드에서 한다. 한 task의 트리는 한 번에 하나만 읽고, 읽는 동안 바뀐 것은 끝난 뒤 다시 읽는다.
- 추적: `agent.summary`에 `queued=`(요청이 에이전트 스레드를 기다린 시간)를 더하고, 같은 크기를 다시 받으면(라이브 다음의 다시 구성) 처음 받은 시각에서 잰다. `service.forwarded live=false` 요약에 `since_live=`(같은 크기의 라이브 요청이 나간 뒤 지난 시간)를 더했다. 에이전트의 각 스레드는 한 일이 250 ms를 넘으면 그 일의 클래스 이름과 함께 경고를 남긴다(10초에 한 줄).
- 최대화: Windows는 최대화 크기를 주 모니터 기준으로 주고, 그 값이 주 모니터 크기 이상일 때만 창의 모니터에 맞춘다. 앱 창은 게스트가 그릴 수 있는 가장 큰 크기로 줄여 주므로(2880x2880, 창 배율로 1803x1800), 주 모니터보다 작은 모니터(2048x1152)에서는 창이 화면 아래로 넘쳐 최대화됐다. 이제 `WM_GETMINMAXINFO`가 창 자신의 모니터 작업 영역으로 줄인다(`maximized_outer`).
- 메모리: 렌더러는 디스플레이마다 텍스처 핸들을 4개까지 쥐고 있었는데, Windows 창 관리자는 디스플레이마다 텍스처를 하나만 쓰고 창이 그것보다 커질 때만 새로 준다. 그래서 크기 구간이 바뀔 때마다 앞 크기의 텍스처가 렌더러에서 살아 있었다(이 측정에서 약 21 MiB, 디스플레이마다 최대 3개). 이제 Windows 렌더러는 새 텍스처를 연 뒤 앞의 것을 바로 닫는다. 렌더러 전용 메모리가 412에서 630 MiB로 는 것의 나머지(약 100-150 MiB)는 Vulkan 할당 수(최대 약 63 MiB 증가)로 설명되지 않고, 창을 닫고 쉬면 350 MiB로 돌아왔다. 붙잡혀 있지만 새지는 않는다. gfxstream의 렌더 스레드별 스트림과 디코드 버퍼가 가장 큰 전송만큼 자라고 줄지 않는다는 것이 남은 의심이다(이 작업 트리에 gfxstream 소스가 없어 확인하지 못했다). VM 상주 메모리와 게스트 사용량의 480 MiB 차이는 게스트 값이 크기 변경 전(51초 전)의 것이라 성립하지 않는다.
- CI: 이미지 부팅 검사에 "Resize a real app's window on the booted image"(`dsa/host/platform/linux/tests/resize.rs`)를 더했다. 소프트웨어 Vulkan에서 설정 앱을 열고 호스트 접근성 클라이언트가 지켜보는 채로, 크기 구간을 가로지르는 8가지 크기를 두 번씩 1.5초 간격으로 창 관리자가 하듯(라이브, 보고되면 다시 구성) 준다. 게스트가 새 크기를 보고하기까지(호스트 시계) 95번째 백분위 150 ms, `queued=` 50 ms, 프레임까지 1000 ms를 넘으면 실패하고, 값은 작업 요약에 남는다.

### 확인하지 못한 것

- 고친 뒤의 실제 값 전부. 빌드하거나 PC에서 실행하지 않았다. PC에서는 같은 조건(Google Play, 1.5초 간격 26번, 크기 변경 추적 켬)으로 `app.summary waited=` 중앙값과 95번째, `agent.summary queued=`, `service.forwarded since_live=`, 에이전트의 `on one piece of work` 경고를 본다. 기대하는 값은 `queued=` 수 ms, 전체는 게스트 쪽 85-140 ms에 호스트 몇 ms다.
- 에이전트 스레드를 막은 것이 접근성 읽기 하나뿐인지. 다른 일이 막았다면 새 경고 줄이 그 클래스 이름을 남긴다. 크기 요청은 이제 줄 맨 앞으로 가므로, 지금 돌고 있는 일 하나만큼만 기다린다.
- FLO가 2880x2876을 처음 그리는 1.1초. 앱의 다시 배치와 큰 첫 프레임이며 이번에 고치지 않았다.
- CI 검사는 Google Play가 아니라 설정 앱으로 한다. 프로젝트는 Google Play를 싣지 않고, CI의 작은 Google 앱 묶음은 서명되지 않아 설치되지 않는다. 설정 앱의 트리는 Google Play보다 작아서, 고치기 전의 코드였어도 이 검사가 PC만큼 느려지지 않았을 수 있다.
- 최대화의 실기 확인. 다중 모니터 꼴은 단위 시험으로, 작업 영역을 채우는 최대화와 복원은 Windows 창 시험(`task_windows.rs`)으로 확인하는데, CI의 Windows 러너는 모니터가 하나다.

## R32. GPU 리셋에 YouTube가 끝난 까닭: 렌더러가 장치를 다시 만드는 13.4초 동안 게스트 호출이 모두 기다렸다 (NFR-5)

**기준.** 소유자 PC(Windows 11, Intel Arc 내장 GPU, 200%와 125% 모니터, v0.2.3)의 `%LOCALAPPDATA%\DSA\logs\dsa-wm.log`(2026-11-09 19:37 UTC와 2026-11-12 00:50 UTC의 리셋)와 같은 시각의 Windows 이벤트 로그를 읽었다. PR #654가 SurfaceFlinger의 복구와 창 프로그램의 다시 시작을 고친 뒤 남은 세 가지를 살폈다.

### 시각

| 시각(UTC, 2026-11-12) | 줄 |
|---|---|
| 00:50:32.899 | task 11, 1536x1536 |
| 00:50:36.930 | 최대화, 텍스처 2880x2048(프레임 2880x1800) |
| 00:50:38.461 | 1536x1536으로 돌아옴 |
| 00:50:38.469 | 그 크기의 복사가 `VK_ERROR_DEVICE_LOST` |
| 00:50:38.470 | 렌더러: 장치를 잃었다, 새로 만든다 |
| 00:50:44.831 | 렌더러 메모리: GPU 전용 0 MiB, 0개(ColorBuffer를 모두 놓았고 아직 하나도 만들지 않았다) |
| 00:50:51.875 | 렌더러: 새 장치, ColorBuffer 34개 중 34개 |
| 00:50:51.901 | 창 관리자: 자기 Direct3D 장치를 새로 만듦(0x887a0007) |
| 00:50:52.894부터 | 게스트 장치들이 `VK_ERROR_DEVICE_LOST`를 받기 시작 |

11-09에는 같은 구간이 23.8초(19:37:47.052-19:38:10.881)였다. `C:\Windows\LiveKernelReports\WATCHDOG`의 수정 시각은 09:50(KST)이고 덤프 파일은 남아 있지 않다.

### 까닭

- 13.4초 내내 렌더러는 `recoverIfLost()` 안에서 장치 잠금과 `mMutex`를 쥐고 있었다(패치 0015). ColorBuffer를 만지는 게스트 호출(만들기, 읽기, 쓰기, 레이아웃, 가져오기)은 모두 그 잠금을 기다렸고, 게스트 장치의 손실 보고도 장치가 돌아온 뒤에야 나왔다(00:50:52.894). 입력에 5초 넘게 답하지 못한 YouTube를 Android가 ANR로 끝냈다.
- 그 시간의 대부분은 GPU가 리셋에서 돌아오기를 기다린 것으로 보인다. 6.3초 시점에 낡은 객체는 이미 다 풀렸고 새 것은 하나도 없었으며, 창 관리자의 Direct3D 장치도 렌더러보다 26 ms 늦게서야 만들어졌다. 남은 후보는 잃은 장치의 `vkDestroyDevice`와 새 `vkCreateDevice`이고, 둘 다 리셋(라이브 커널 덤프 쓰기 포함)이 끝날 때까지 막힌다. 단계별 시간은 새 로그 줄이 나눠 보여 준다.
- 그 밖에 잠금을 쥔 채 기다리는 곳: 의심 장치 검사의 `vkQueueWaitIdle`(끝없음), ColorBuffer 읽기의 5초 다음 10초, 다른 복사의 5초.
- 창 관리자는 렌더러가 다른 프로세스(`dsa-gfx`)일 때 장치 세대를 늘 0으로 읽었다(`RendererClient`에 `device_generation`이 없었다). 그래서 리셋을 자기 Direct3D 장치가 실패할 때에야 알았다.
- 리셋 자체의 원인은 확정하지 못했다. 장치가 2초 넘게 응답하지 않은 일(TDR)이 최대화 직전부터 시작됐다(감지에 2초가 걸리므로 늦어도 36.47초). 11-09와 11-08, 11-10의 리셋도 창 크기가 바뀌거나 Play 스토어가 열릴 때였다. DSA가 그 크기에서 내는 GPU 일을 살폈다.
  - 창 텍스처로의 복사(`dsa_gfx_display.cpp`): 2880x1800 블릿 하나. 내장 GPU의 대역폭으로 1 ms 안팎이다.
  - ColorBuffer 읽기와 쓰기(`readColorBufferToBytes`, `updateColorBufferFromBytes`, 0019의 YUV 올리기): 이미지 전체를 복사 하나로. 2880x2048 RGBA는 23.6 MB다.
  - QSRI(`on_vkQueueSignalReleaseImageANDROID`): 게스트가 준 세마포어를 GPU에서 기다린다. 같은 큐에서 게스트가 신호하는 세마포어이며, WDDM에서 신호되지 않은 동기 객체를 기다리는 큐는 엔진이 아니라 스케줄러에서 멈추므로 TDR을 일으키지 않는다. 게스트 Vulkan만 쓰는 DSA에서는 스테이징 복사가 없는 경로(`mUseVulkanNativeImage`)를 탄다.
  - 호스트 0020과 mesa 0009, 0010: CPU와 스트림 쪽 일이며 GPU 일을 내지 않는다. mesa 0002, 0006의 되읽기는 게스트 큐의 `vkQueueWaitIdle`(CPU 대기)이다.
  - 어느 것도 2초 가까이 걸릴 크기가 아니다. 남는 후보는 게스트 앱과 SurfaceFlinger가 그 크기에서 처음 그리는 일(호스트에서는 게스트 장치의 일)과 드라이버다. 확인하려면 리셋 때의 라이브 덤프나 GPUView 추적이 필요하다.

### 고친 것

- 패치 0021: 잃은 장치는 렌더러 자신의 스레드에서 다시 만든다(`dsa_device_remake.h`). 그동안 장치가 필요한 호출은 곧바로 실패한다(게스트는 `VK_ERROR_DEVICE_LOST`나 실패한 ColorBuffer 호출을 받는다). 잃은 장치를 부수고 새 장치를 만드는 일은 잠금 없이 하고, 아직 만들 수 없으면 50 ms부터 1초까지 간격을 늘려 다시 해 본다. ColorBuffer는 하나씩 잠금을 따로 잡아 다시 만든다. 잠금 안의 GPU 대기는 모두 50 ms 조각으로 3초까지만 기다리고, 장치를 잃은 것이 알려지면 곧바로 그친다. 의심 장치 검사는 빈 제출의 펜스를 100 ms까지만 기다린다.
- 렌더러 프로세스는 손실과 세대 수를 50 ms마다 읽어 바뀌면 `DeviceState`(renderer.proto)로 보낸다. 손실 수는 장치를 잃은 것을 안 순간 오른다. 창 관리자는 이를 GPU 리셋으로 받아(500 ms 주기) 창을 지키고 자기 장치를 살핀다.
- 창 텍스처로의 복사(`dsa_gfx_display.cpp`)와 ColorBuffer 읽기, 쓰기, 버퍼 복사(패치 0022)는 2 Mpixel(버퍼는 8 MiB)을 넘지 않는 띠로 나누어 띠마다 따로 제출하고 기다린다(`dsa_copy_bands.h`). 2880x1800은 세 개다.

### 확인하지 못한 것

- 빌드, C++ 시험, 실기. 이 PC에서 빌드하거나 실행하지 않았다. CI의 Graphics test가 패치가 붙는지와 시험을 처음 돌린다.
- GPU 리셋 뒤 게스트 호출이 실제로 곧바로 실패하는지와 새 로그 줄의 단계별 시간. PC에서 같은 조건(YouTube 창 최대화, 또는 `dsa_gfx_simulate_device_loss`)으로 본다.
- 리셋의 원인. 위의 살핌으로는 DSA의 GPU 일에서 찾지 못했다.
- 손으로 쓴 생성 코드. 이 PC에는 `protoc`이 없어 `dsa.renderer.v1.rs`와 해시를 생성기와 같은 규칙으로 직접 썼다(해시는 `schema` 크레이트의 정규화를 그대로 옮겨 셈했고, 고치기 전 정의로 기존 해시가 나오는 것을 확인했다). CI의 생성 코드 검사가 다르다고 하면 Protocol sync로 다시 만든다.

## R33. v0.1.9와 develop의 직접 비교: 창 크기 변경과 메모리 (FR-4, NFR-1, NFR-2)

**기준.** v0.1.9(`f1286665`)와 develop(`72851f35`, PR #662)를 git으로만 비교했다(`log`, `diff`, `show`, `merge-base`). 빌드하거나 실행하지 않았고 설치된 DSA와 바탕 화면은 건드리지 않았다. 숫자는 R5, R26, R29, R31의 실측과 커밋 메시지에서 옮겼다. 릴리스 커밋: v0.1.10 `d2aefc03`, v0.2.0 `02aa4dd4`, v0.2.1 `78b15555`, v0.2.2 `148adc49`, v0.2.3 `091c0238`.

### 바로잡을 것: 느려진 시점은 v0.2.0이다

R26과 R29는 guest/mesa 0005(`94262816`)가 v0.2.0에 없다고 적었지만, `git merge-base --is-ancestor 94262816 02aa4dd4`로 보면 v0.2.0에 들어 있다(PR #525 앞의 develop). v0.1.10에는 없다. 소유자의 "v0.1.x는 빨랐고 v0.2.x부터 느리다"와 맞는다. v0.2.0을 쓰는 동안 크기 변경 기록이 없었을 뿐이다(R29). v0.1.10과 v0.2.0 사이에 크기 변경 경로에 들어온 것은 셋이다: guest/mesa 0002-0005(PR #459, #469), 시스템 배율 방식(`cc64e0cd`, PR #528), TSF(PR #494, 크기 변경 경로 밖).

### 크기 변경 경로에서 달라진 것 (지연을 더할 수 있는 순서)

| 순위 | 차이 | 들어온 곳 | 근거와 크기 |
|---|---|---|---|
| 1 | 앱이 펜스가 신호된 것을 볼 때마다 게스트 드라이버가 호스트 가시 메모리를 다시 읽는다 | mesa 0005 (v0.2.0), 0007 (v0.2.2), 0010과 호스트 0020 (v0.2.3) | R29: 앱 프레임 중앙값 150 ms, 크기 한 번 718 ms(v0.2.2). 0010/0020 뒤 게스트 쪽 85-140 ms(R31). v0.1.9의 전체는 54-80 ms. 남은 비용은 펜스마다의 동기 왕복 하나와, 렌더러가 매핑된 할당 전체를 4 KiB마다 해시하는 일이다. 해시는 모든 렌더 스레드가 함께 쓰는 잠금 하나(`dsa_shadow_record.h`의 `mutex_`) 안에서 한다 |
| 2 | 크기 요청이 에이전트 스레드에서 접근성 트리 읽기 뒤에 줄을 섰다 | 접근성 내보내기는 v0.1.9 전부터(`c0bcc66a`, PR #165) | R31: 0.1-1.4초. v0.1.9에도 같은 줄이 있었다. 앱의 UI 스레드가 1번 때문에 늦게 비면서 트리 읽기가 길어진 것으로 본다(추론). PR #653이 고쳤고 실기에서 재지 않았다 |
| 3 | 화면 주사율이 가장 빠른 모니터를 따른다(60-240 Hz) | `8d75c724`, `22d212be` (PR #451, v0.1.10) | v0.1.10은 빨랐으므로 혼자서는 원인이 아니다. 프레임마다 1번의 비용이 들므로 초당 그 비용이 2-4배가 된다(추론) |
| 4 | 시스템 배율 방식에서 창 크기와 태스크 크기의 반올림 왕복이 1 px 어긋난다 | `cc64e0cd` (PR #528, v0.2.0) | 요청한 크기의 프레임이 창 크기로 세지지 않았다. 창은 끈 크기에서 1 px 다른 크기를 받고, 지연 측정은 그 기다림을 닫지 못해 표본을 잃고 프레임이 왔는데도 "no WxH frame came in 2009 ms"를 남겼다. 이번에 고쳤다(아래) |
| 5 | 밀도가 160 x 배율에서 131 x 배율로 | PR #560, #623 (v0.2.3) | 같은 픽셀의 창이 dp로 약 1.22배 넓다. 앱이 그리는 내용이 늘고 크기 구간 경계가 다른 곳에 온다. 소유자가 정한 규칙이다 |
| 6 | hwcomposer가 CPU가 쓴 층(영상, 카메라)을 올린다 | `faaba031` (PR #591, v0.2.2) | SurfaceFlinger 스레드가 `TaskComposer::mMutex`를 쥐고 층의 펜스를 최대 3초 기다린다. 그런 층이 있을 때만 |
| 7 | 태스크 읽기마다 `getActivityInfo` 바인더 호출 | PR #528, #560, #637 | 태스크마다 한두 번, 수 ms 이하 |

바뀌지 않은 것: 호스트의 `FrameFollow`, 크기 조절 페이서(150 ms), `STUCK` 2초, `SETTLE` 2초, `CLAIM_WAIT` 100 ms, `DwmFlush`. 드래그 단계는 모두 라이브 `Resize`이고 끝에 한 번만 다시 구성한다. 드래그 중에 `SetTaskDensity`를 보내지 않는다. 에이전트의 `resizeTaskLive`와 hwcomposer의 정착 시한(500 ms, 2000 ms, 512 단계)도 같다. Android 판(17 `16373615`가 기본)과 렌더링 경로(ANGLE, 게스트 Vulkan)도 같다.

80 ms가 0.5-2초가 된 것은 1과 2가 겹친 것으로 설명된다. 1은 앱의 한 프레임을 수십에서 수백 ms로 늘렸고, 그동안 UI 스레드가 늦게 비어 2의 트리 읽기가 길어졌으며, 크기 요청은 그 뒤에서 기다렸다. 3은 1의 비용을 초마다 늘렸다. 4는 실제 지연보다 로그의 "프레임이 오지 않았다"를 만들었다. v0.2.3의 R31 측정은 1의 고침 뒤이고 2의 고침 앞이다.

### 메모리에서 달라진 것

| 차이 | 들어온 곳 | 비용 |
|---|---|---|
| 창 버퍼의 ColorBuffer를 앱 프로세스가 끝날 때까지 놓지 않았다 | mesa 0002-0005 이후 드러남, 고침 mesa 0009 (PR #621, v0.2.3) | 렌더러 GPU 할당 v0.1.9 66-111 MiB에서 v0.2.1 567-931 MiB, 최대 1731 MiB(R26). 내장 GPU에서는 호스트 RAM이다. 고친 뒤는 재지 않았다 |
| 게스트가 호스트 가시 메모리의 사본을 0으로 채워 상주시켰다 | patches/0003부터, 0005로 커짐, 고침 0007 (PR #558, v0.2.2) | 재지 않았다 |
| Google Play가 더하는 것 | v0.1.9에도 있던 선택 기능 | 게스트 사용 +560-1010 MiB, PSS +400 MiB, 캐시 +290-650 MiB(R26). 줄이는 변경(PR #572-#578, #576)은 재지 않았다 |
| 페이지 캐시 다듬기의 바닥이 되읽힌 만큼 오른다(최대 1 GiB) | `06e9b75e` (PR #621, v0.2.3) | 재지 않았다. 바닥은 되읽히지 않은 다듬기마다 64 MiB씩만 내려가고 앱을 닫아야 256 MiB로 돌아간다 |
| VM 시작 크기 3072에서 2048 MiB, 늘이기 재시도 | PR #633, #562, #564 | struct page -16 MiB. 늘리는 기준(가용 < max(1/4, 768 MiB))은 같아서 Google Play가 있으면 곧 3 GiB로 큰다 |
| 커널: zram 재압축, 쓰지 않는 파일 시스템 빼기, 파일 기반 erofs | PR #622, #573 | 추정 -30에서 -40 MiB, -3 MiB |
| 이미지: 예제 HAL 13개와 얼굴·지문 HAL, TeleService 빼기, 캐시 프로세스 8 | PR #622, #633, #572 | PSS -16.3 MiB, -8.5 MiB. 상한 8은 v0.1.9의 Android 17에서 적용되지 않았다(1024) |
| 앱 창 프로그램이 TSF 클라이언트가 됨 | PR #494 (v0.2.0) | 앱 창 38 MiB(R8)에서 33-71 MiB. TSF의 몫은 재지 않았다 |
| 줄지 않는 버퍼: 게스트 `HvSocketStream`의 버퍼, gfxstream 렌더 스레드의 읽기·쓰기 버퍼, mesa 0008과 hwcomposer의 스테이징 | patch 0001(전부터), 큰 전송은 0005, 0007, PR #591부터 | 스레드마다 가장 큰 전송만큼 자라고 줄지 않는다. 렌더러의 private 814와 상주 598 MiB 차이(R26)의 후보다. 재지 않았다 |

v0.1.9 때도 렌더러는 따로 된 프로세스(`dsa-gfx`)였고, 앱 창 프로그램은 앱마다 하나였다. 이 둘은 늘어난 까닭이 아니다. 가장 큰 몫은 Google Play와 렌더러의 GPU 할당이고, 렌더러 쪽은 v0.2.3의 고침(mesa 0009, 홈 액티비티 1x1, 층 없는 태스크의 버퍼 놓기)이 실기에서 재지지 않았다.

### 고친 것

시스템 배율 방식의 반올림(위 4): 창 크기나 마지막으로 요청한 크기가 바뀐 태스크 크기의 프레임은 그 창 크기로 센다(`window_size_of`). 다른 프레임은 전처럼 배율로 바꾼 크기다. Windows의 `present.rs`와 core의 `WindowSizing`(게스트 보고, 정착 시한)에 적용했다. 시험: `fr4_a_scaled_frame_of_the_asked_size_fills_exactly_the_size_dragged_to`. Linux 창 관리자(`toplevel.rs`의 `frame_in_window`, `wayland/gpu.rs`, `wayland/window.rs`)도 같게 했다(`fr4_a_scaled_frame_of_the_proposed_size_fills_exactly_that_size`). macOS는 층의 배율로 그리고 창 크기를 되돌려 셈하지 않으므로 해당하지 않는다.

### 확인하지 못한 것

- 모든 값. PC에서 같은 조건(배율이 다른 모니터, Google Play, 크기 변경 추적 켬)으로 `app.summary waited=`, `agent.summary queued=`, "no WxH frame came" 줄의 수를 PR #653과 이 고침 앞뒤로 재야 한다.
- 1의 남은 비용 가운데 해시가 차지하는 몫. 입력 추적 부팅의 `driver.readback ms_per_read`가 그것을 보인다.
- Linux 창 관리자의 `wayland` 모듈은 Linux에서만 컴파일되므로 이 PC의 컴파일 확인에 들지 않았다. CI의 Linux 검사가 처음 컴파일한다.

## R34. Play의 일회성 프로세스가 캐시로 계속 남은 까닭: 캐시 상한의 절반은 빈 프로세스 몫이고 나이 정리는 1000시간이다 (NFR-1, NFR-2)

**기준.** 소유자 PC의 v0.2.3 진단 묶음(2026-11-12 14:04 UTC, FLO 창 표시, APKPure 백그라운드, 게스트 가용 869 MiB로 메모리 압박 없음)의 `android/dumpsys.txt`(`dumpsys activity processes`, `dumpsys meminfo`), `android/logcat.txt`, `host/android-console.log`를 읽었다. 프레임워크 동작은 AOSP `frameworks/base` main의 `ActivityManagerConstants.java`, `OomAdjuster.java`, `ProcessList.java`, `ActivityManagerService.java`로 확인했다.

### 남은 프로세스

| 프로세스 | 시작(UTC) | 까닭 | 상태 | PSS |
|---|---|---|---|---|
| com.android.vending | 14:01:34 | `PhoneskyJobServiceMain`(Play 작업) | CEM, adj 915 | 107.1 MiB |
| com.google.android.gms.unstable | 14:04:00 | `DroidGuardService`(Play가 묶음) | CEM, adj 905, 동결됨 | 68.2 MiB |
| com.android.vending:background | 13:47:55 | `BackgroundProcessEndpointService` | CEM, adj 905 | 40.1 MiB |
| com.android.settings | 13:48:30 | `BatteryUsageBroadcastReceiver` | CEM, adj 905 | 32.0 MiB |

넷의 PSS 합은 247.4 MiB(`Cached` 253,342K)다. 오래 사는 Google 프로세스는 `com.google.android.gms`(BFGS, adj 100, 시스템이 묶음) 하나다. vending UID는 `bg:+2m59s idle`이었다.

### 상한이 듣지 않은 까닭

- 상한 8은 적용됐다. 잠금 해제(12:58:33) 뒤 10분 동안은 `OomAdjuster: Not killing cached processes`가 찍히고(`no_kill_cached_processes_post_boot_completed_duration_millis` 기본 600000), 13:08:39에 `empty #5`부터 `empty #14`까지 열 개가 끝났다.
- 그러나 `updateAndTrimProcessLSP`는 빈 프로세스를 `CUR_MAX_EMPTY_PROCESSES = 8 / 2 = 4`개까지 남긴다. 넷 모두 빈 프로세스(CEM)이고 수가 정확히 4라서 다섯째가 생기기 전에는 아무것도 끝나지 않는다.
- 나이로 정리하는 길은 `CUR_TRIM_EMPTY_PROCESSES`(2)를 넘는 빈 프로세스가 `max_empty_time_millis`보다 오래 쉬었을 때인데, 그 기본값이 `1000L * 60 * 60 * 1000`, 1000시간이다.
- 에이전트의 `activity kill-all`은 태스크가 하나도 없이 화면이 꺼질 때와 호스트가 메모리가 부족하다고 할 때만 돈다. FLO 창이 떠 있었고 호스트도 부족하지 않았다.

### 고친 것

에이전트(`memory/CachedProcesses.kt`)가 15초마다 프로세스 목록을 보고, 캐시 상태로 30초 머문 프로세스 가운데 활동이 없고, 패키지에 태스크와 포그라운드 서비스가 없는 것을 끝낸다. gms 본체(`com.google.android.gms`, `.persistent`, `com.google.process.gapps`), system, persistent, 에이전트는 끝내지 않는다. 끝낸 뒤 곧 돌아오는 프로세스는 다음에 두 배 오래(8분까지) 기다린다.

끝내는 방법은 `ActivityManager.killBackgroundProcesses`다. 다른 방법을 견줬다.

- `forceStopPackage`: 작업, 알람, 알림을 지우고 패키지를 멈춤 상태로 만든다. gms에 쓰면 FCM이 끊긴다. 쓰지 않는다.
- 프로세스 하나를 끝내는 `killPids`, `killApplicationProcess`, `killProcessesBelowForeground`: system UID만 부를 수 있다.
- `killProcessesWhenImperceptible`(FORCE_STOP_PACKAGES): 기기가 유휴(Doze)에 들어갈 때만 끝내므로 창이 열린 동안은 듣지 않는다.
- `killBackgroundProcesses`: 패키지 상태, 작업, 알람을 그대로 두고 점수 500 이상의 프로세스만 끝낸다. 작업 스케줄러와 gms의 푸시가 필요할 때 앱을 다시 띄운다. 다만 패키지 단위이고 그 패키지의 코드를 불러온 다른 프로세스(`packageDependencies`, 예: WebView를 쓰는 최소화된 앱)도 끝낸다. 그래서 끝내기 전에 `dumpsys activity processes`의 기록을 읽어, 그 호출이 닿을 프로세스(여유를 두어 점수 250 이상)가 모두 이 정책이 어차피 끝낼 것일 때만 부른다.

결정을 `dsa-channeld`가 아니라 에이전트에 둔 까닭: 프레임워크를 거쳐 끝낼 수 있는 것은 KILL_ALL_BACKGROUND_PROCESSES를 가진 에이전트이고, 채널 데몬은 lmkd처럼 신호를 보내는 수밖에 없어 활동 관리자의 기록을 건너뛴다. 태스크 창 목록도 에이전트에 있다.

### 기대하는 절감

- PSS로 247.4 MiB(위 표). PSS는 zram에 들어간 몫을 압축 전 크기로 센다.
- 실제 비용: 채널 데몬의 마지막 pageout 줄(vending 상주 익명 17, 스왑 58 MiB; vending:background 16, 45; gms.unstable 15, 42)과 측정한 압축률 3.76:1(zram 571,232K를 152,112K에)로 셈하면 vending 약 64 MiB(PSS 107에서 58을 빼고 58/3.76을 더함), gms.unstable 약 37 MiB, vending:background 16-29 MiB, Settings 32 MiB 이하로, 합 약 150-160 MiB다. 소유자의 `dsa memory` 수치(62, 29, 110, 33)로는 234 MiB다.
- Google Play 몫은 gms 본체(38 MiB)와 그것이 묶은 서비스만 남아 100 MiB 아래로 내려간다. 끝난 프로세스의 익명 메모리와 zram은 바로 풀려 빈 페이지 보고로 호스트에 돌아가고, 파일 페이지는 페이지 캐시로 남았다가 다음 회수 때 돌아간다.

### 확인하지 못한 것

- Kotlin 빌드와 시험, 실기. 이 PC에서 빌드하거나 실행하지 않았다.
- 실제 기기에서 `processState`, `flags` 숨은 필드를 읽는지, `dumpsys activity processes`를 에이전트가 읽는지(DUMP, platform_app의 파이프를 system_server가 쓰는 정책은 이미 있다), 끝낸 줄과 절감량.
- Play 작업이 끝난 프로세스를 얼마나 자주 다시 띄우는지(다시 띄우는 CPU 비용).

## R35. 앱 창 하나에서 게스트의 "unaccounted" 215 MiB와 클라이언트 타깃 (NFR-1)

**기준.** v0.2.3, Android 17 `16373615-14` 이미지, 디스플레이 2880x2880, FLO 창 하나와 위젯 하나, Google Play 있음. 소유자가 모은 `logs-mem-now.zip`(dumpsys meminfo, dumpsys SurfaceFlinger, logcat 12:58-14:04)과 `mem-now.json`(`dsa memory`의 값)만 읽었다. AOSP frameworks/native `ae266dcb`의 파일과 Linux v6.18의 `hv_balloon.c`, `hyperv_transport.c`, `netvsc_drv.c`를 받아 읽었다. 빌드하거나 실행하지 않았다.

### 클라이언트 타깃이 놓이지 않은 까닭

dumpsys가 주 디스플레이의 표면을 `LegacyFramebufferSurface`로 적는다. Android 17의 SurfaceFlinger는 `wb_framebuffersurface2` 플래그로 `FramebufferSurface`와 `LegacyFramebufferSurface` 가운데 하나를 고르고(`SurfaceFlinger.cpp`의 `processDisplayAdded`), 이 빌드는 플래그가 꺼져 있어 늘 레거시 쪽이다. frameworks/native 0004는 `FramebufferSurface`에만 놓기를 넣었으므로 한 번도 돌지 않았다. 그래서 클라이언트 합성이 없는데(`usesClientComposition=false`, 모든 층 DEVICE) 2880x2880 버퍼 둘(32411 KiB씩)이 남았고, "its buffers are freed" 줄이 logcat에 없었다. Android 16의 `FramebufferSurface`는 레거시와 같은 ConsumerBase 클래스여서 0004가 그대로 듣는다.

### 게스트 RAM이 아닌 것: gralloc 버퍼

meminfo의 DMA-BUF 109,836K는 minigbm이 vkms에서 만든 dumb 버퍼다. vkms의 GEM은 shmem이고 페이지는 처음 쓸 때 생긴다. 게스트에서 그 페이지를 쓰는 것은 CPU가 쓰거나 읽는 버퍼뿐이고(렌더링은 호스트 ColorBuffer로 간다), 같은 때 Shmem 전체가 24,428K다. 그러므로 gralloc 버퍼가 차지하는 게스트 RAM은 많아야 24 MiB이고 그것도 "page cache"에 들어간다. "unaccounted"에는 들지 않는다. 클라이언트 타깃 63 MiB와 FLO의 버퍼 22 MiB는 렌더러 프로세스의 ColorBuffer(호스트 메모리)다.

FLO의 BLAST 버퍼 5개: `VRI[MainActivity]#1`의 4개는 HWUI가 창마다 잡는 수(꺼내지 않는 하나 더하기 셋)다. `#29`의 1개는 같은 창 이름의 두 번째 창(액티비티에 붙은 대화 상자, SurfaceFlinger 층 `#472`가 `#96`을 다 덮는다)이다. 닫힌 태스크의 버퍼는 목록에 없다. 위젯이 보이는 동안의 `ImageReader-663x278` 둘과 `VRI[]#3` 넷(합 5.3 MiB)은 위젯이 쓰는 것이다.

### "unaccounted"를 이루는 것 (추정)

`unaccounted_kb`는 MemTotal - MemFree에서 Cached, Buffers, AnonPages, SReclaimable, SUnreclaim, KernelStack, PageTables, VmallocUsed, Percpu, Hugetlb, zram 사용량을 뺀 값이다(`memory.rs`). `mem-now.json`에서 196,572K, 같은 VM이 2048 MiB이던 때(`idle.txt`) 61 MiB다.

| 후보 | 크기 | 근거 |
|---|---|---|
| 핫 애드된 메모리의 struct page | 약 28 MiB | 온라인 3840 MiB 가운데 부팅 뒤 늘어난 1792 MiB, 4 KiB마다 64바이트. 부팅 때의 몫은 MemTotal 밖이다 |
| VMBus 링 버퍼 | 수 MiB | hv_sock 연결마다 최소 24 KiB씩 두 방향(`RINGBUFFER_HVS_*`, gfxstream은 버퍼 크기를 바꾸지 않는다), netvsc 채널마다 512 KiB씩 두 방향 |
| SwapCached, KReclaimable에서 SReclaimable을 뺀 것, binder 버퍼 페이지, CPU별 free 페이지 목록 | 재지 않았다 | 이 값들은 보고에 없어서 모두 "unaccounted"에 들어간다 |

`hv_balloon`은 풍선으로 내준 페이지를 MemTotal에서 뺀다(`adjust_managed_page_count`). 핫 애드에서 아직 덮이지 않은 페이지도 MemTotal 밖이다. 그래서 `reserved_at_boot_kb`(온라인 - MemTotal)는 핫 애드 뒤 1,389,360K로 커지고, "부팅 때 커널이 잡은 것"이 아니라 풍선과 덮이지 않은 핫 애드를 함께 센다.

### 고친 것

- frameworks/native 0005(PR #668): `LegacyFramebufferSurface`에 0004와 같은 놓기를 넣었다. 마지막 클라이언트 합성 1초 뒤의 합성 없는 프레임에서 현재 버퍼를 돌려주고 큐의 빈 버퍼를 버리며, HWC가 본 버퍼 ID를 잊고, RenderSurface가 텍스처를 놓게 한다. 시험: `test_nfr1_android17_frees_the_client_target_whichever_framebuffer_surface_it_picks`. Framework build가 develop에서 돈 뒤에야 이미지에 들어간다.
- hwcomposer(PR #670): 호스트에 다시 붙는 동안 온 클라이언트 프레임도 세어, 클라이언트 타깃을 놓을 프레임을 2초 뒤에 요청한다(`ClientTargetUse`). 시험: `nfr1_a_client_frame_asks_for_the_frame_that_frees_the_client_target`.

기대: 렌더러의 ColorBuffer 63 MiB(2880x2880 둘, 디스플레이가 작으면 그만큼 적다)와 게스트 dumpsys의 DMA-BUF 63 MiB. 게스트 RAM은 거의 줄지 않는다(위).

### 확인하지 못한 것

- 모든 값. 새 Framework 이미지에서 클라이언트 합성(영상 재생) 뒤 logcat의 "its buffers are freed"와 dumpsys의 `LegacyFramebufferSurface` 슬롯이 비는지, 렌더러 private이 63 MiB 줄어드는지 재야 한다.
- 이 빌드에서 클라이언트 합성 782 프레임(`frame-counter=782`)이 언제 무엇 때문에 생겼는지. logcat에 SurfaceFlinger와 hwcomposer의 정보 줄이 거의 없다.
- "unaccounted"의 나머지(약 160 MiB). 게스트에서 `/proc/meminfo`의 SwapCached와 KReclaimable, `/proc/vmstat`의 `nr_memmap_pages`, `/proc/zoneinfo`의 pageset `count`, binder 통계를 함께 읽어야 나뉜다.

## R36. Google Play가 DSA에 Chrome 101(32비트 브라우저 빌드)을 내려 준 까닭 (FR-11, FR-25, FR-32)

**기준.** 소유자 PC의 DSA(Android 17 `16373615`, 지문 `Samsung/dsa_x86_64/dsa_x86_64:17/CP2A.260605.016/...`, 모델 `NT960XGP-G72AG`)를 adb로 읽기만 했다(`getprop`, `pm list features`, `pm list libraries`, `logcat -d`, `/proc/meminfo`). 게스트는 `ro.product.cpu.abilist=x86_64,arm64-v8a`, `ro.system.product.cpu.abilist32`가 빈 값, `ro.dalvik.vm.isa.arm64=x86_64`, `ro.zygote=zygote64`, `MemTotal` 2,542,800 kB다. 비교 대상은 이 PC의 SDK에 있는 `system-images/android-35/google_apis/x86_64`(`AE3A.240806.043`, `abilist32` 빈 값)다. `google_apis_playstore/x86_64`는 받다 만 폴더(`.installer`뿐)라 비교하지 못했다. 이미지는 `.scratch/chrome-trichrome/`에 복사해 7-Zip 24.08로 GPT, super, ext4를 차례로 풀고, APK는 aapt2(build-tools 36.0.0)로 읽었다. 버전 코드는 Chromium `build/util/android_chrome_version.py`(main)의 표로 풀었다. 버전 코드는 `{빌드}{패치 3자리}{패키지 1자리}{ABI 1자리}`이고, ABI 자리의 표는 빌드 5750에서 바뀐다. 작업이 끝난 뒤 복사본은 지웠다.

### 본 것

- Play가 설치한 Chrome은 `chromium-Monochrome.aab-stable-495106126`, 101.0.4951.61이다. 4951 / 061 / 2 / 6: 빌드가 5750보다 작아 옛 표를 쓰고, 패키지 자리 2는 MONOCHROME, ABI 자리 6은 intel `32_64`(브라우저는 32비트 x86, WebView만 64비트 x86_64)다. 32비트 ABI가 없는 기기에서 Android는 이 앱을 x86_64로 설치한다. x86_64 라이브러리에는 WebView만 있어서, Chrome은 시작하자마자 "Starting in 64-bit mode requires the 64-bit native library"로 끝난다(`am_crash` 세 번). Trichrome 라이브러리는 설치되지 않았다. Monochrome은 라이브러리 없이 혼자 도는 형식이기 때문이다.
- 에뮬레이터 이미지의 Chrome은 `product/app/Chrome`, `TrichromeLibrary`, `WebViewGoogle`(각각 `.apk.gz`와 `-Stub`)이고, 모두 124.0.6367.219, 버전 코드 636771938이다. 6367 / 219 / 3 / 8: TRICHROME, 새 표의 intel `64_32`(브라우저 64비트, WebView 32비트)다. Chrome과 TrichromeLibrary는 `lib/x86_64`에 코드(`libmonochrome_64.so` 129.7 MB)를, `lib/x86`에 빈 `libplaceholder.so`를 둔다. WebViewGoogle은 반대로 `lib/x86`에 코드, `lib/x86_64`에 빈 자리표시자를 둔다. 셋 다 `multiArch=true`이고 `use32bitAbi`가 없으며, minSdk 29, targetSdk 34다. 에뮬레이터에서 Chrome이 되는 까닭은 이미지가 64비트 브라우저 빌드를 미리 설치해 두기 때문이다. 이 PC의 AVD(`skylens35`, `google_apis`, RAM 2G)에는 Play가 없으니, 에뮬레이터의 결과는 Play가 무엇을 고르는지에 대해서는 아무것도 말하지 않는다.
- 사용자가 APKPure로 받은 `.xapk`는 Chrome 151.0.7922.174, 792217431이다. 7922 / 174 / 3 / 1: TRICHROME, 새 표의 arm `32_64`다. 기본 APK는 `lib/armeabi-v7a`에 코드, `lib/arm64-v8a`에 빈 자리표시자를 두고 `use32bitAbi=true`, `multiArch=true`, targetSdk 36이다. 패키지 관리자는 "The multiArch app's native libs don't support all the natively supported ABIs of the device"로 거부했다. Android 15부터 `forceMultiArchNativeLibsMatch` 플래그가 켜져 있고 targetSdk가 35 이상이며 ABI 지정이 없으면, multiArch 앱은 기기 고유 ABI마다 라이브러리를 가져야 한다(`PackageAbiHelperImpl`). 기기 고유 ABI는 `ro.dalvik.vm.isa.<isa>`가 없는 ABI라, DSA에서는 x86_64뿐이다. arm64는 번역이라 빠진다.
- 따라서 Play가 arm64 전용 multiArch 앱(targetSdk 35 이상)을 내려 주면 DSA의 패키지 관리자는 번역이 켜져 있어도 거부한다. 호스트 설치는 ABI를 지정하므로(`setAbiOverride`) 이 검사를 받지 않는다.
- 기능 비교: 에뮬레이터는 `android.hardware.bluetooth`, `bluetooth_le`, `location.gps`, `location.network`, `telephony.*`, `software.midi`를 더 알린다. 그러나 Chrome 101의 매니페스트(태그 `101.0.4951.61`)도 151도 `uses-permission-sdk-23 BLUETOOTH`로 `android.hardware.bluetooth`를 암묵적으로 요구한다(aapt2의 `uses-feature-sdk-23`). 두 버전의 조건이 같으므로, 기능 차이로는 101만 받은 것을 설명할 수 없다. Chrome이 쓰는 공유 라이브러리 가운데 DSA에 없는 것도 없다. `com.google.android.trichromelibrary`는 정적 라이브러리라 Play가 함께 준다.
- APKMirror에는 2026년의 Chrome에도 x86 + x86_64 빌드가 있다. 예를 들어 Trichrome Library Dev 788500038은 7885 / 000 / 3 / 8, intel `64_32`다. 그러니 DSA에서 돌 64비트 브라우저 빌드는 Play에 있는데, Play가 DSA에 주지 않는다.
- 메모리: Chrome은 64비트 브라우저를 RAM 8 GB 이상인 Android 10 이상 기기에만 주고, 나머지에는 32비트 브라우저 빌드(`32_64`)를 준다고 알려져 있다(2021년 출시 보도). DSA 게스트의 `MemTotal`은 2.4 GiB다. 그래서 RAM으로는 32비트 브라우저 빌드의 대상이고, 32비트 ABI가 없어 현재의 `32_64` 빌드는 맞지 않으며, 그런 제한이 없던 마지막 빌드인 Monochrome 101 `32_64`가 남았다고 본다. 이것은 증거에 맞춘 설명이지 확인한 것이 아니다. Play의 대상 지정은 서버에 있어 읽을 수 없다.

### 고친 것

- 호스트 설치(FR-11의 설치 창, 끌어 놓기, `dsa install`, `dsa apk-info`)는 Android가 보기 전에 네이티브 코드를 읽는다. 빈 파일은 코드로 치지 않는다. 64비트 코드(x86_64, 또는 번역을 거친 arm64-v8a)가 없고 32비트 코드나 다른 ABI의 코드만 있으면, 또는 `use32bitAbi`인데 64비트 쪽이 32비트 쪽의 라이브러리를 빠뜨리면, `unsupported-abi`로 거부하고 이유와 받을 빌드를 말한다. 정적 공유 라이브러리를 쓰고 64비트 ABI에 자리표시자만 둔 앱(Trichrome WebView `64_32`)은 64비트 코드가 라이브러리에 있으므로 받는다. 위 `.xapk`는 이제 Android에 가기 전에 이유와 함께 거부된다.
- Play로 설치된 앱은 호스트가 설치 전에 볼 수 없다. 대신 에이전트가 Chromium의 위 메시지로 끝난 앱도 알리고, 호스트는 그 앱의 APK가 32비트 코드를 갖고 ARM 실행으로도 길이 없으면 64비트판을 설치하라는 알림을 띄운다(FR-25의 알림과 같은 경로). Monochrome 101은 x86과 x86_64에 같은 이름의 라이브러리를 두므로, 이름 비교가 아니라 이 메시지로 알아본다.

### 사용자가 할 수 있는 것

- Play의 Chrome(Monochrome 101)을 지우고, 같은 버전의 Trichrome Library와 Chrome 짝을 DSA의 APK 설치로 설치한다. 버전 코드가 `38`로 끝나는 것(intel `64_32`, x86 + x86_64)이다. 라이브러리를 먼저 설치한다. ARM 번역이 켜져 있으면 `34`로 끝나는 arm `64` 짝(arm64-v8a 전용)도 되지만, 번역이라 느리다.
- 이렇게 설치한 Chrome은 Play가 이 기기에 맞는 빌드를 주지 않는 한 Play로 업데이트되지 않는다.

### 소유자가 정할 것(제안, SPEC에 없다)

- 게스트가 알리는 RAM을 8 GB 이상으로 하는 것. 위 설명이 맞다면 Play가 64비트 브라우저 빌드를 준다. 비용: VM 메모리 크기를 키우면 커널의 페이지 표(`struct page`)만 1.6%다(8 GiB에 128 MiB). 프레임워크가 보고하는 값만 키우면 앱이 그만큼 캐시를 크게 잡는다. 둘 다 메모리 목표와 부딪친다.
- FR-25가 ARM 번역기를 받는 것처럼, 사용자가 고를 때 Google x86_64 에뮬레이터 이미지에서 Chrome과 TrichromeLibrary를 꺼내 Google 앱 추가 구성 요소(FR-32)에 넣는 것. 이미지의 빌드가 64비트 브라우저 빌드이므로 DSA에서 돈다. 새 요구사항이다.
- `forceMultiArchNativeLibsMatch`를 끄거나, 번역되는 ABI도 기기 고유 ABI로 세게 하는 것. 프레임워크를 바꾸는 일이라 빌드 전에 소유자의 승인이 필요하다.

### 확인하지 못한 것

- RAM 가설. Play의 서버 쪽 대상 지정은 읽을 수 없고, 메모리를 바꾼 게스트로 시험하지 않았다.
- 실기. 빌드하거나 실행하지 않았다. 64비트 빌드 짝이 DSA에서 열리는지, 알림이 Chrome 101의 첫 시작에 뜨는지, `.xapk`가 설치 창에서 이유와 함께 거부되는지는 PC에서 본다.
## R37. 그래픽 렌더러(dsa-gfx)가 앱 하나에 655 MiB를 잡은 까닭 (NFR-1, NFR-2)

- 대상: v0.2.3, 소유자 PC(Intel Arc 내장 GPU, Windows 11). FLO 창 하나에서 dsa-gfx가 private 655 MiB, 상주 205 MiB였다. 로그는 2026-11-12의 logs-mem-now(dsa-wm.log의 렌더러 줄)이고, 렌더러 코드는 고정 리비전에 0001-0022를 적용한 gfxstream에서 읽었다. 이 PC에서 빌드나 실행은 하지 않았다.
- 방법: 렌더러가 스스로 세는 줄(GPU-only, CPU-mapped, imported)과 프로세스의 private을 시간 순으로 맞추고, 각 몫이 생기고 풀리는 코드를 따라갔다.
- 결과:

| 찾은 것 | 근거 | 고침 | 예상 절감 |
| --- | --- | --- | --- |
| 기기를 다시 만든 뒤 셈이 0이 되고, 게스트 기기의 할당은 처음부터 세지 않았다 | GPU 리셋(11384행) 뒤 "GPU-only 0 MiB in 0"인 채 private 399에서 983 MiB. makeDeviceObjectsLocked가 디스패치 표를 드라이버 값으로 다시 채운다. CPU-mapped는 모든 줄에서 0 | 패치 0023과 dsa_gfx_memory.cpp(PR #674) | 없음(측정을 바로잡는다) |
| 시작부터 128 MiB 스테이징 버퍼 | kDefaultStagingBufferSize를 시작 때와 기기를 다시 만들 때마다 할당·매핑했다. CPU가 색 버퍼를 읽고 쓸 때만 쓴다. 게스트 없이 시작한 렌더러가 private 195-201 MiB | 패치 0026(PR #680): 복사할 때 그 크기로 만들고 5초 쉬면 놓는다 | 최대 128 MiB |
| 닫힌 창의 색 버퍼가 다음 창이 올 때까지 남았다 | 0009의 retire 목록은 다음 색 버퍼를 만들거나 없앨 때만 본다 | 패치 0025와 1초 정리 스레드(PR #677) | 창 하나 몫(2880x1800 창에서 약 100 MiB) |
| VM이 끝나도 렌더러가 남은 것을 안고 다음 부팅을 맞았다 | 게스트 0개에서 336, 456 MiB(새 렌더러는 199). pid 64884가 Android 재시작을 넘겼다 | ServedVm(PR #683): 섬기던 VM이 사라지면 렌더러가 끝나고 새로 뜬다 | 137-257 MiB(로그 기준) |
| 렌더 스레드의 읽기 버퍼와 디코더 BumpPool이 가장 큰 전송의 두 배로 자라고 줄지 않았다 | ReadBuffer::getData, aemu BumpPool::freeAll | 패치 0027, aemu 0003(PR #681) | 스레드마다 가장 큰 전송의 두 배, 재지 않았다 |
| DeviceOpTracker가 펜스와 세마포어를 흘렸다 | "Leaking object" 130번, "leaking garbage objects on destruction" 112번. 폴링 전에 게스트가 리셋한 펜스의 작업은 영원히 대기로 보였다 | 패치 0024(PR #675) | 바이트로는 작다 |

- 남은 것: 게스트 프로세스마다 호스트에 VkDevice가 하나씩 있고(ANGLE), 드라이버의 기기별 몫과 ANGLE의 버퍼 풀은 이번 고침에 들지 않는다. 0023 뒤의 CPU-mapped 줄이 그 크기를 처음 보여 준다. 위 절감은 모두 실기에서 재지 않았다. 다음 측정에서 같은 조건(FLO 창 하나, 1분)으로 렌더러 줄을 비교한다.
- 결정: 위 고침을 develop에 넣었다. 기기별 몫을 줄일지(기기 공유 등)는 측정 뒤에 따로 정한다.

## R38. 비일관 캐시 메모리 형식과 일관 쌍둥이: ANGLE이 어느 형식을 고르는가 (PR-3, FR-4)

**기준.** R33의 제안 1(소유자 승인): v0.1.9의 싼 경로(비일관 캐시 형식, 앱의 `vkInvalidateMappedMemoryRanges`에서만 되읽기)를 되살리면서 Vulkan이 요구하는 HOST_VISIBLE|HOST_COHERENT 형식을 지킨다. ANGLE은 이미지 입력 빌드 16373615(Android 17)에 든 것을 `external/angle`의 `android-17.0.0_r1`(`1f4500d3`) 소스로 읽었다. 빌드하거나 실행하지 않았다.

**한 일.** guest/mesa 0012는 호스트가 캐시하는 호스트 가시 형식 T마다 둘을 보인다. T는 호스트가 보인 그대로(비일관) 제 인덱스에 두고, T|COHERENT 쌍둥이를 호스트의 형식들 뒤에 붙인다(`dsa_memory_types.h`). 호스트의 인덱스와 마스크는 그대로 게스트의 것이므로 색 버퍼 인덱스처럼 호스트가 준 값은 바뀌지 않는다. 쌍둥이 인덱스는 호스트로 갈 때 T가 되고, 호스트의 마스크에 T가 있으면 쌍둥이도 들어간다. 완료 시점(펜스, 세마포어, 큐와 장치 대기)의 되읽기는 일관 형식만 한다. Vulkan의 순서 규칙(플래그가 엄격한 부분 집합인 형식이 앞)은 호스트의 어떤 형식도 COHERENT가 없으므로 지켜진다.

**ANGLE이 고르는 형식.** ANGLE은 VMA로 고르고, VMA는 필수 플래그를 가진 형식 가운데 선호 플래그를 덜 놓친 것을, 같으면 앞의 것을 고른다.

| 경로 | ANGLE이 묻는 것 | 고르는 형식 |
|---|---|---|
| 텍스처 올리기 스테이징 (`CachedNonCoherent`) | 필수 HV+HC, 선호 없음 | T (비일관). 쓰고 `flush`한다 |
| 내부 `DynamicBuffer` (정점, 인덱스 변환, 기본 유니폼) | 필수 HV, 선호 HC 또는 없음 | 앞의 HV 형식 (T 또는 캐시 없는 형식) |
| `glReadPixels`와 장치 버퍼 매핑의 스테이징 (`CachedPreferCoherent`) | 필수 HV+HC, 선호 COH | 쌍둥이. 드물고, 읽기 전에 `invalidate`를 부른다 |
| GL 버퍼 `DYNAMIC_DRAW`, `STREAM_DRAW` | 필수 HV, 선호 COH(+HC) | 쌍둥이. 바꿀 ANGLE 스위치가 없다 |
| GL 버퍼 `DYNAMIC_READ`, `STREAM_COPY` 등 | 스위치에 따라 HV+HC 또는 HV+COH+HC | 이미지가 `preferCachedNoncoherentForDynamicStreamBufferUsage`를 켜서 T |
| GL 버퍼 `STATIC_*` | 내장 GPU면 DL+HV+COH 선호(`preferDeviceLocalMemoryHostVisible`), 아니면 DL | 이미지가 그 스위치를 꺼서 DL 전용 형식 |

HWUI는 `SkiaOpenGLPipeline`으로 ANGLE 위에서 그리므로(R19) 위 표를 그대로 따른다. 글리프 아틀라스와 비트맵 올리기는 T로, Skia의 정점, 인덱스, 유니폼 스트리밍(`DYNAMIC_DRAW`, `STREAM_DRAW`)은 쌍둥이로 간다. 장치는 그 버퍼에 쓰지 않으므로 완료 때의 되읽기는 호스트가 크기 0으로 답하는 왕복 하나와 렌더러의 조각 해시만 남는다(R33의 남은 비용). ANGLE은 비일관 형식에서 `flush`와 `invalidate`를 명시적 범위로 부르며, 그 범위는 `nonCoherentAtomSize`로 올림한다.

**함께 고친 것.** 비일관 형식의 사용자는 범위를 `flush`하고 `invalidate`한다. 인코더의 `vkFlushMappedMemoryRanges`는 `VK_WHOLE_SIZE`를 오프셋과 상관없이 매핑 크기만큼 보내 게스트 사본의 끝을 넘어 읽었고, 호스트 렌더러는 `VK_WHOLE_SIZE` 범위를 오프셋에서 할당 크기만큼 읽고 보내 매핑 끝을 넘는다. 0012는 보낼 바이트를 사본 안으로 자르고, 그림자 메모리의 `VK_WHOLE_SIZE` 범위를 호스트 할당 끝까지의 바이트 수로 바꾸어 보낸다.

**확인하지 못한 것.** 모두 CI와 실기의 몫이다. `dEQP-VK.info.device_memory_properties`, `dEQP-VK.memory.allocation.basic.*`, `dEQP-VK.memory.mapping.suballocation.full.*`(비일관 형식의 `flush`와 `invalidate`, 쌍둥이 할당)가 CTS 부분 집합에 있다. 크기 변경 지연과 `driver.readback` 줄의 변화는 재지 않았다. 호스트가 HV만 있는(캐시 없는) 형식을 보이는 GPU(NVIDIA 등)에서는 그 형식이 Vulkan이 허용하는 플래그 조합이 아니며(HV는 COH나 HC와 함께여야 한다), 이 변경 전부터 그렇다. ANGLE 속성(`debug.angle.feature_overrides_*`)이 앱 프로세스에서 읽히는지는 이미지에서 확인하지 않았다.

## R39. 읽기 전용 시스템 이미지를 게스트가 캐시하지 않고 호스트에서 매핑하기: 가상 영구 메모리와 DAX (NFR-1, NFR-2)

**질문.** 게스트는 읽기 전용 파티션(system, system_ext, product, vendor, 추가 기능 erofs 이미지)을 자기 페이지 캐시에 읽어 두고, Windows는 같은 이미지 파일을 자기 파일 캐시에 둔다. 같은 바이트가 두 벌이고, 게스트 쪽 벌은 VM이 잡는 메모리다(소유자 PC에서 약 200 MiB, R26의 페이지 캐시 214-741 MiB 가운데 일부). 읽기 전용 디스크를 Hyper-V 가상 영구 메모리(vPMEM)로 붙이고 게스트가 DAX로 마운트하면 게스트 캐시 없이 호스트의 매핑에서 바로 읽는가. 무엇이 필요하고, 무엇을 잃고, 얼마나 주는가.

**기준.** 이 조사에서 빌드하거나 실행하지 않았다. 설치된 DSA와 바탕 화면은 건드리지 않았다. 읽은 것은 아래 원본과 소유자 PC의 파일 크기다.

| 자료 | 버전 |
|---|---|
| HCS 스키마 참조(`VirtualPMemController`, `VirtualPMemDevice`, `VirtualPMemMapping`, `VirtualPMemBackingType`, `VirtualPMemImageFormat`) | learn.microsoft.com, 2026-11-11에 읽음 |
| hcsshim `internal/hcs/schema2/virtual_p_mem_*.go`, `internal/uvm/create_lcow.go`, `internal/vm/vmutils/constants.go`, `internal/guest/storage/pmem/pmem.go` | `main`, 2026-11-11 |
| WSL 커널 `Microsoft/config-wsl`(= `arch/x86/configs/config-wsl`), `arch/arm64/configs/config-wsl-arm64`, `drivers/acpi/nfit/core.c`, `drivers/nvdimm/{pmem.c,namespace_devs.c,pfn_devs.c,virtio_pmem.c,nd_virtio.c}`, `drivers/dax/super.c`, `drivers/md/dm-verity-target.c`, `fs/erofs/{inode.c,super.c}` | `linux-msft-wsl-6.18.54.1`(erofs, dax, dm는 같은 6.18 원본) |
| crosvm `src/crosvm/cmdline.rs`, `devices/device_virtio_pmem/src/lib.rs`, `Cargo.toml` | `b1aa4d87`(`versions.env`의 고정 리비전) |
| AOSP `build/make/tools/releasetools/build_image.py` | `main`, 2026-11-11 |
| Cuttlefish `aosp_cf_x86_64_only_phone-target_files-15581820.zip`의 `META/misc_info.txt`, `IMAGES/`, 파일 목록 | Android 16, 소유자 PC의 `.scratch/aosp-inputs` |
| 소유자 PC의 이미지 `C:\ProgramData\DSA\images\images\17.0.0-16373615-13\system.vhdx` | 크기만 읽음 |

### Windows: HCS의 가상 영구 메모리

1. 스키마. `VirtualMachine.Devices.VirtualPMem`은 `VirtualPMemController`다: `Devices`(번호에서 `VirtualPMemDevice`로, 2.0), `MaximumCount`(uint8, 2.0. 0이 아니면 `HostPath` 없는 빈 장치를 그 수까지 더한다), `MaximumSizeBytes`(2.0. 빈 장치의 크기, 없으면 512 MB), `Backing`(`Virtual` 또는 `Physical`, 2.1). 장치는 `HostPath`, `ReadOnly`, `ImageFormat`(`Vhdx` 또는 `Vhd1`, 2.0), `SizeBytes`와 `Mappings`(2.2, 한 장치에 여러 파일을 겹쳐 놓는 기능)를 가진다. 스키마 2.0과 2.1은 Windows 10 1809 SDK부터이므로 DSA가 쓰는 2.3 문서와 Windows 11 클라이언트에서 쓸 수 있는 필드다.
2. Microsoft 자신의 쓰임새(hcsshim, 리눅스 컨테이너용 유틸리티 VM). 루트 파일 시스템을 `root=/dev/pmem0`으로 부팅하고, 장치 0을 `HostPath` = rootfs.vhd, `ReadOnly: true`, `ImageFormat: "Vhd1"`(확장자가 vhdx일 때만 `Vhdx`)로 둔다. 기본 장치 수는 64(최대 128), 기본 크기는 4 GiB이고 `Backing`은 정하지 않는다. arm64에서는 장치 수를 0으로 해서 vPMEM을 쓰지 않는다. 게스트 쪽(`pmem.go`)은 ext4를 `MS_RDONLY`, `noload`로 마운트하고 DAX를 쓰지 않으며, dm-linear와 dm-verity를 pmem 위에 둔다. 곧 Microsoft는 vPMEM을 계층 공유에 쓰고, DAX는 쓰지 않는다. DSA가 DAX로 쓰는 것은 Microsoft가 보여 준 적 없는 조합이다.
3. 형식. vPMEM은 파일을 VM 주소 공간에 그대로 매핑하므로 평면 이미지가 필요하다. `Vhd1`(고정 VHD: 디스크 바이트 뒤 512바이트 꼬리말)은 hcsshim이 쓰는 형식이다. 이미지의 `system.vhdx`는 동적 VHDX라 그대로 쓸 수 없다. 사용자의 `addons.vhd`는 이미 고정 VHD다.
4. 크기 정렬. 스키마에는 정렬 규칙이 적혀 있지 않다. 게스트 쪽에서는 리눅스가 영구 메모리를 2 MiB 단위(subsection)로 매핑하고, 영역의 시작이나 크기가 그 단위가 아니면 페이지 구조를 만들지 못해 DAX가 꺼진다(`devm_memremap_pages`의 범위 검사, `nd_pfn_validate`의 `memremap_compat_align`). 그래서 디스크를 2 MiB 배수로 만든다.
5. Windows가 매핑한 페이지를 세는 방법. `Backing: "Virtual"`은 파일을 매핑한 것이고 `Physical`은 VM 메모리로 채운 것이라는 이름이지만, 문서에 설명이 없다. 매핑한 페이지는 깨끗한 파일 페이지이므로 vmmem 작업 집합에 들어와도 Windows가 대기 목록으로 내려 다시 읽을 수 있을 것으로 본다. 여러 사용자의 VM이 같은 이미지를 열면 한 벌을 나눠 쓸 것이다. 둘 다 확인하지 못했다(아래).

### 게스트 커널

1. WSL 설정에 이미 있는 것: `LIBNVDIMM=y`, `BLK_DEV_PMEM=y`(arm64는 `m`), `VIRTIO_PMEM=y`, `DAX=y`, `FS_DAX=y`, `FS_DAX_PMD=y`, `ZONE_DEVICE=y`, `MEMORY_HOTREMOVE=y`, `SPARSEMEM_VMEMMAP=y`, `ND_PFN=y`. 없는 것: `ACPI_NFIT`(Hyper-V vPMEM을 알리는 ACPI 표의 드라이버)이 두 아키텍처 모두 `m`이다. 이미지는 모듈을 싣지 않으므로 `android.config`에 `ACPI_NFIT=y`, `BLK_DEV_PMEM=y` 등을 적었다.
2. 원시 이름 공간의 DAX. `pmem.c`는 pfn 장치이거나 `pmem_should_map_pages()`가 참일 때만 블록 장치에 DAX를 준다. 그 함수는 영역에 `ND_REGION_PAGEMAP`이 있어야 참인데, `virtio_pmem.c`는 이 표시를 붙이고 `acpi/nfit/core.c`는 붙이지 않는다. 그래서 Hyper-V의 원시 이름 공간은 그대로는 DAX가 없다. 다른 길인 pfn 정보 블록(`NVDIMM_PFN_INFO`, 이름 공간 4 KiB 지점)은 장치에 써야 만들어진다(`nd_pfn_init`은 읽기 전용 영역이면 `-ENXIO`). 여러 VM이 나눠 쓰는 읽기 전용 이미지에는 쓸 수 없다. 그래서 패치(`pmem-dax.patch`)가 NFIT의 PM 영역에 `ND_REGION_PAGEMAP`을 붙인다. 이때 페이지 구조(4 KiB마다 64바이트, 1.56%)는 게스트 메모리에 놓인다.
3. by-name 링크. Android의 첫 단계 init과 ueventd는 부팅 장치 아래의 디스크에만 `/dev/block/by-name` 링크를 만들고, 디스크의 부팅 장치는 경로를 거슬러 처음 만나는 플랫폼 장치(또는 PCI 슬롯)로 정한다. NFIT의 NVDIMM 버스는 ACPI 장치 아래라 링크가 없다(VMBus가 그랬다, `vmbus-platform-device.patch`). virtio-pmem은 PCI 아래지만 crosvm이 pmem 장치의 슬롯을 스스로 고르고(`--pmem`에 `pci-address`가 없다), 정할 수 없는 슬롯을 부팅 장치로 적을 수 없다. 그래서 같은 패치가 두 버스를 각각 플랫폼 장치 `nfit`, `virtio-pmem` 아래에 둔다. NFIT의 DSM 호출과 알림은 계속 `acpi_desc->dev`(ACPI 장치)로 가고, virtio-pmem의 플러시는 영역의 `provider_data`(virtio 장치)로 가므로 부모를 바꿔도 깨지지 않는다.
4. erofs의 DAX(6.18). `dax`, `dax=always`, `dax=never` 마운트 옵션이 있고, 블록 크기가 페이지 크기여야 하며("unsupported blocksize for DAX"), 장치가 DAX를 못 하면 "DAX unsupported by block device. Turning off DAX."를 남기고 옵션을 지운 채 마운트한다. 파일마다는 `S_ISREG`이고 배치가 `EROFS_INODE_FLAT_PLAIN` 또는 `EROFS_INODE_CHUNK_BASED`일 때만 DAX다. 압축한 파일과 마지막 블록을 inode에 넣은 파일(FLAT_INLINE)은 이전처럼 캐시된다. 파일 기반 마운트(R26의 add-on 이미지)는 DAX 장치를 얻지 않는다.
5. device-mapper. dm-linear는 DAX를 통과시키고(`linear_dax_direct_access`, 파티션 시작을 더한다), dm-verity는 `direct_access`가 없다. 논리 파티션(super 안의 system 등)은 dm-linear만 두면 DAX가 되고, dm-verity를 두면 안 된다. `fs_dax_get_by_bdev`는 파티션의 시작과 크기가 페이지 배수가 아니면 "unaligned partition for dax"로 거절한다. sgdisk는 1 MiB, liblp는 1 MiB로 정렬하므로 맞는다.

### Android의 마운트 표와 검증

1. 마운트 표는 `androidboot.fstab_suffix`로 고른다. 첫 단계는 bootconfig, 그다음 커널 명령줄에서 읽지만, 두 번째 단계와 ueventd는 `ro.boot.*` 속성을 읽고 그 속성은 명령줄이 먼저 정한다(읽기 전용 속성이라 처음 값이 남는다). 그래서 호스트는 bootconfig에 더하지 않고 명령줄의 `androidboot.fstab_suffix`와 `androidboot.boot_devices`를 그 자리에서 바꾼다.
2. 이미지는 `fstab.dsa`에서 `fstab.dsa_dax`를 만든다. 논리 파티션 항목만 바뀐다: 마운트 옵션에 `dax=always`, fs_mgr 플래그에서 `avb`, `avb=...`, `avb_keys=...`를 뺀다. `/data`, `/metadata`, misc, zram, boot 계열의 항목은 그대로다.
   - 처음 쓴 설명("부팅이 vbmeta와 boot 이미지의 해시를 이전처럼 확인한다")은 틀렸다(보안 검토 M1). boot 계열 항목은 `recoveryonly`라 첫 단계 마운트가 건너뛰고, 첫 단계 init은 dm-verity로 마운트하는 항목이 있을 때만 검증 부팅(`AvbHandle::Open`)을 연다. 논리 파티션이 모두 `avb`를 잃으면 vbmeta를 확인하지 않고 `ro.boot.avb_version`도 정하지 않는다.
   - 그래서 `odm`(수백 KiB, 해시 트리는 최상위 vbmeta에 있다)은 항목을 그대로 두어 dm-verity로 마운트한다(`DIRECT_MAPPED_VERIFIED`, `image.json`의 `direct_mapped.verified_partitions`). 호스트가 만드는 디스크도 `odm`의 바이트를 해시 트리째 그대로 둔다. 이제 첫 단계가 vbmeta와 연결된 vbmeta, boot 계열의 해시를 보통 부팅처럼 확인한다. Image test의 측정은 두 모드의 `ro.boot.avb_version`과 커널 로그의 AVB, dm-verity 줄을 견주고, 매핑된 부팅에 값이 없거나 다르면 실패한다(실행 전, 확인하지 못함).
3. 검증. DAX와 dm-verity는 함께 쓸 수 없다. erofs에는 fs-verity가 없다. 그래서 이 방식에서는 매핑하는 파티션의 블록 검증을 게스트가 하지 않고 호스트가 맡는다: 디스크는 서비스가 서명된 목록으로 확인한 `system.vhdx`에서 서비스(SYSTEM)가 만든 사본이고, 만든 뒤와 서비스가 시작할 때마다 서명된 `image.json`의 SHA-256과 바이트 단위로 견준다(아래 "호스트가 만드는 디스크"). SYSTEM만 쓸 수 있는 폴더에 있고, VM에 읽기 전용으로 붙으며, 게스트 커널도 영구 메모리 디스크를 읽기 전용으로 둔다(`ND_REGION_READONLY`). 부팅은 이미 `verifiedbootstate=orange`, `device_state=unlocked`이므로 부팅 상태가 바뀌지 않는다. 잃는 것은 호스트 파일이 부팅 뒤 바뀌었을 때 게스트가 그것을 알아차리는 것이다. 이 설정을 기본으로 켜기 전에 소유자가 이 맞바꿈을 정해야 한다.

### 이미지의 압축

1. 공개 빌드(Android 16 `15581820`)의 `misc_info.txt`: `erofs_default_compressor=lz4hc,9`, `erofs_default_compress_hints`는 `0 .*\.apex$`(APEX만 압축하지 않음). 이미지 크기는 system 729 MB, system_ext 257 MB, product 250 MB, vendor 304 MB(합 약 1.55 GB)이고, 같은 파티션의 파일은 압축 전 959, 347, 364, 297 MiB(합 약 1.98 GiB)다. 곧 지금의 이미지에서는 APEX 말고는 DAX가 되지 않는다.
2. `make-image.py --uncompressed-erofs`는 `misc_info.txt`에서 압축기를 `none`으로, 압축 힌트와 파티션별 압축기, pcluster 크기를 빼고, `erofs_share_dup_blocks`(`build_image.py`에서 `--chunksize 4096`)를 켠다. 4 KiB 청크 배치는 FLAT_INLINE을 피하고(erofs가 DAX로 매핑하는 배치) 같은 블록을 한 번만 저장한다.
3. 막는 것: 소유자 PC의 `system.vhdx`(Android 17, 압축)는 1,803,550,720바이트다. 압축을 풀면 약 0.4 GiB가 늘어(위 비율, DSA가 앱을 빼고 odex를 더하는 몫은 재지 않았다) 2.2 GB 안팎이 되고, GitHub 릴리스 자산 하나의 한도(2 GiB)를 넘을 것으로 본다.
4. 결정(소유자, 2026-11-13): 릴리스 이미지는 지금처럼 압축해 내려받고, 압축하지 않은 DAX용 디스크는 PC가 스스로 만든다. 내려받는 크기는 그대로다.

### 호스트가 만드는 디스크

1. 무엇을 만드는가(`dsa_host_core::direct_mapped_disk`, 형식 1). 원본 `system.vhdx`의 가상 디스크(동적 VHDX를 그대로 읽는다, `dsa_host_core::vhdx`)에서 GPT와 super 앞의 작은 파티션(vbmeta, boot 계열)은 같은 자리에 그대로 두고, super의 liblp 메타데이터를 다시 쓴다. erofs 논리 파티션은 압축을 푼 사본으로 바꾼다: 같은 트리, 모드, 소유자, 시간, 링크 수, 확장 속성(SELinux 레이블, 파일 능력), 모든 inode를 확장 inode와 FLAT_PLAIN으로. `verified_partitions`(`odm`)와 erofs가 아닌 파티션은 그대로 복사한다. 파티션은 블록 장치 정렬(1 MiB)로 붙여 놓고, super는 마지막 파티션 뒤에서 끝난다. 원본 super는 8 GiB(`super_partition_size`, 대부분 빈 공간)라 그대로 두면 영구 메모리의 페이지 구조가 약 128 MiB가 되므로 줄인다. 그래서 커널 명령줄의 `blkdevparts`에서 super의 크기를 호스트가 바꿔 준다. 디스크는 2 MiB 배수, 뒤에 GPT 백업, Windows는 고정 VHD 꼬리말(식별자는 해시에서 만든다)을 붙인다.
2. 읽는 erofs 형식(커널 6.18 `fs/erofs`를 따랐다): FLAT_PLAIN, FLAT_INLINE, CHUNK_BASED, 압축 FULL과 COMPACT(4B, 2B 팩) 색인, LZ4(0 패딩, 부분 해제), PLAIN과 interlaced 클러스터, big pcluster, tail packing. 압축 프래그먼트, LZ4 외의 알고리즘, 긴 xattr 접두사, 4 KiB가 아닌 블록, 추가 장치는 거절한다. 거절된 이미지는 이 모드 없이 부팅한다.
3. 같은 바이트. 만드는 방법이 결정적이므로 이미지 빌드(`dsa-release direct-mapped`, release-image와 Image test의 이미지 작업)가 같은 사본을 만들어 그 SHA-256과 크기, super 크기를 `image.json`의 `direct_mapped.system_disk`에 적는다. `image.json`은 서명된 목록에 자기 해시로 실려 있으므로 이 해시도 서명된다. 목록 자체에 칸을 더하지 않은 까닭: 지금까지 나간 서비스는 목록의 모르는 키를 거절한다(`deny_unknown_fields`). 같은 방식으로 `system_disk_raw`(디스크 그대로의 해시)도 적어, Linux의 원시 사본(`system.img`)을 그것과 견준다(보안 검토 L5).
4. 서비스(`DirectMappedImages`, 서비스에 하나). 서비스가 시작할 때와 이미지를 내려받아 확인한 뒤 현재 이미지의 사본을 본다. 이전 실행이 만든 사본은 먼저 바이트 단위로 해시를 다시 견준다. 없거나 덜 됐거나 틀리면 만든다: 구간(디스크 앞, super 메타데이터, 파티션 하나씩, 끝)마다 `sync`하고 기록해, 멈춘 실행은 다음에 이어 간다. 다 만들면 `sync_all`하고 다시 읽어 해시를 견준 뒤에야 이름을 붙인다(보안 검토 H2). 틀리면 지우고 처음부터 한 번 더, 그래도 틀리면 이 모드 없이 부팅한다. 다른 이미지의 사본은 지운다. Windows는 `THREAD_MODE_BACKGROUND_BEGIN`(유휴 CPU, 아주 낮은 I/O와 메모리 우선순위), Linux는 nice 19와 유휴 I/O 등급으로 돈다. 사본은 Windows `%ProgramData%\DSA\direct-mapped\<version>.vhd`(SYSTEM만 쓰고, 관리자는 읽고 지우며, VM 그룹 `S-1-5-83-0`은 읽기만), Linux `/var/lib/dsa/direct-mapped/<version>.img`(root 0755, 파일 0644)에 둔다. 한 번에 하나만 돈다(보안 검토 L2).
5. 부팅. 이번 서비스 실행이 확인한 사본만 쓴다. 확인 중이면 최대 20초 기다리고, 아직 없으면 그 부팅은 이미지 자신의 디스크(SCSI 또는 virtio-blk, dm-verity)로 하고 만들기를 재촉한다. 압축된 이미지 그대로는 DAX로 붙이지 않는다(보안 검토 L4).
6. 시간과 자리(추정, 재지 않았다). 소유자 PC(Intel 노트북, SSD)의 Android 17 이미지: 압축된 데이터 약 1.6 GB를 읽고 약 2.0 GiB를 LZ4로 풀어 약 2.3 GB를 쓰고, 다시 2.3 GB를 읽어 SHA-256을 낸다. LZ4 풀기는 한 코어에서 초당 0.5-1 GB, 쓰기와 읽기는 SSD에서 초당 0.5-2 GB, SHA-256은 SHA 확장이 있는 CPU(Ice Lake 이후)에서 초당 1.5 GB 안팎, 없으면 0.4-0.5 GB로 보아, 한가한 PC에서 30초에서 1분 반, 다른 일이 디스크를 쓰면 낮은 우선순위 때문에 몇 분이다. 서비스가 시작할 때마다 하는 다시 확인은 2.3 GB 읽기와 해시로 2-6초다. 디스크는 사본만큼(약 2.3 GB) 더 쓰고, 만드는 동안 따로 더 쓰지 않는다(부분 파일이 곧 사본이다). 서비스 로그가 예상 크기, 15초마다 진행률과 남은 시간, 끝난 시간을 남기고, CI의 `dsa-release direct-mapped`가 러너에서 만든 시간을 로그에 적는다.

### 다른 호스트

- Linux(crosvm). `--pmem path=...,ro=true`. crosvm은 파일을 읽기 전용으로 매핑하고, 크기를 2 MiB 배수로 맞춘 매핑 영역을 만들어 끝을 익명 메모리로 채운다. 서비스가 만든 원시 디스크(꼬리말 없음)와 `addons.img`를 붙인다.
- macOS(Virtualization.framework). virtio-pmem이나 DAX 창(virtiofs의 공유 메모리 영역)에 해당하는 장치가 없다. 블록 장치로 남는다.

### 바꾼 것

- 커널: `android.config`에 영구 메모리와 DAX 설정, `patches/pmem-dax.patch`(NFIT 영역의 `ND_REGION_PAGEMAP`, NFIT와 virtio-pmem 버스의 플랫폼 부모, 새 영역 표시 `ND_REGION_READONLY`로 모든 영구 메모리 영역과 그 디스크를 읽기 전용으로). 시험 `test_nfr1_the_system_images_can_be_mapped_from_persistent_memory_with_dax`, `test_nfr1_the_pmem_patch_hunks_count_their_lines`, `test_nfr1_persistent_memory_disks_are_read_only_in_the_guest`.
- 이미지: 모든 이미지에 `fstab.dsa_dax`(파티션과 첫 단계 램디스크, `odm`은 dm-verity 그대로), `image.json`의 `"direct_mapped": {"fstab_suffix", "uncompressed_erofs", "verified_partitions", "system_disk"}`와 `system_disk_raw`, 시스템 디스크를 2 MiB 배수로, `--uncompressed-erofs`(연구용으로 남김), `check-image.py`의 확인. Google 앱과 번역기 스크립트에 `gapps_image=dax`, `arm_translation_image=dax`일 때 각 이미지의 파티션에서 `dax=always`로 마운트하는 동작. 시험 `test_nfr1_*`(make-image).
- 추가 기능 디스크(호스트 공통): 각 이미지의 바이트를 덮는 GPT 파티션 `dsa_gapps`, `dsa_arm_translation`을 erofs 파티션 안에 겹쳐 둔다(리눅스 GPT 파서는 겹침을 막지 않는다). 디스크 크기를 2 MiB 배수로. 보통 부팅은 이전처럼 파일에서 마운트하고 새 파티션을 쓰지 않는다. 이전 DSA의 디스크는 이 모드로 처음 부팅할 때 다시 쓴다.
- 호스트: 설정 `direct_mapped_images`(기본 꺼짐, Windows `dsa-hostd.json`, Linux 설정 파일). 위의 "호스트가 만드는 디스크"가 사본을 만들고 확인한다(처음 안의 `CreateVirtualDisk`로 부팅 때 고정 VHD를 만들던 방식은 해시를 확인하지 않고 8 GiB를 다 써서 버렸다). Windows는 x86_64에서만 HCS 문서에 `VirtualPMem`(장치 0 시스템, 장치 1 추가 기능, `ReadOnly`, `Vhd1`, `Backing: Virtual`)을 두고 그 디스크들을 SCSI에서 뺀다. Linux는 `--pmem`. 둘 다 명령줄의 부팅 장치에 `nfit` 또는 `virtio-pmem`을 더하고, 표를 `dsa_dax`로, `blkdevparts`의 super 크기를 사본의 것으로 바꾼다. 사본이 없으면 그 부팅은 이전처럼 붙인다.
- 측정: Image test의 "Measure the direct-mapped read-only disks against the cached ones on the booted image". 공개하는 그대로의 이미지로, 서비스가 사본을 만들고 확인할 때까지(최대 15분) 기다린 뒤 Android를 다시 시작해 DAX로 붙이고, Google Play와 창 하나를 연 채 재고, 서비스를 설정을 끈 채 다시 띄워 같은 것을 잰다. 매핑된 부팅에서 `/sys/block/pmem*/ro`가 모두 1인지, `dd` 쓰기 시도 뒤에도 호스트 사본의 해시가 같은지, `ro.boot.avb_version`이 이미지 자신의 디스크로 한 부팅과 같은지도 본다. 이미지 작업은 사본의 erofs를 `fsck.erofs`로 검사하고, 원본과 함께 풀어 파일, 모드, 소유자, 링크가 같은지 견준다(실패해도 다른 확인을 막지 않는다). 따로 만들던 압축하지 않은 이미지(`direct-mapped-image`)는 없앴다. 게스트 페이지 캐시, 사용량, VM 상주와 private, crosvm의 익명/파일/공유 페이지를 작업 요약에 남긴다. 매핑된 부팅이 `/system`, `/vendor`, `/product`, `/system_ext`, `/mnt/dsa-gapps`를 `dax=always`로 마운트하지 못했으면 실패한다.

### 기대하는 절감

- 게스트 페이지 캐시에서 읽기 전용 파티션과 Google 앱 이미지의 몫이 빠진다. 소유자 PC의 약 200 MiB 가운데 압축하지 않은 이미지에서는 대부분, 지금의 압축 이미지에서는 APEX 몫만이다(APEX 페이로드는 loop와 dm-verity 위 ext4라 그 안쪽은 여전히 캐시된다).
- 대신 게스트가 매핑한 디스크의 페이지 구조를 잡는다: 압축하지 않은 시스템 디스크 약 2.2 GB에 약 34 MiB, Google 앱 디스크 약 0.6 GB에 약 9 MiB. 이것은 되돌릴 수 없는 커널 메모리다.
- 그래서 VM이 잡는 몫은 약 100-170 MiB 주고(추정), 같은 페이지는 Windows 파일 캐시에 한 벌만 남을 것으로 본다. 앱이 열려 있을 때 이미지의 실행 코드를 다시 읽는 일(R26의 되읽기)은 게스트 회수의 대상이 아니게 된다.

### 확인하지 못한 것

- 실제 Hyper-V에서 되는지 전부. Windows 11 클라이언트의 WSL이 아닌 HCS VM이 `VirtualPMem`과 `Vhd1`을 받는지, `MaximumSizeBytes`가 파일이 있는 장치의 크기를 제한하는지, NFIT가 어떤 모양(라벨 없는 원시 이름 공간인지, 읽기 전용 표시)으로 오는지, 영역의 물리 주소가 2 MiB에 맞는지. 소유자 PC에서 설정을 켜고 부팅해 콘솔 로그의 `nd_pmem`, `pmem0`, erofs의 DAX 줄과 `/proc/self/mountinfo`의 `dax=always`를 보고, `dsa memory`로 게스트 캐시와 vmmem 상주, private를 끈 때와 견줘야 한다. 작업 관리자가 매핑된 파일 페이지를 vmmem의 메모리로 보이는지, 메모리가 모자랄 때 Windows가 그것을 대기 목록으로 내리는지도 그때 본다.
- ARM64 Windows. hcsshim이 vPMEM을 끄므로 DSA도 쓰지 않는다. 되는지 시험한 적은 없다.
- CI의 첫 측정값. 이 변경의 develop 실행이 처음 남긴다. 측정이 끝나기 전에는 설정을 기본으로 켜지 않는다.
- 호스트가 만드는 디스크 전부. 이 컴퓨터에서는 빌드도 시험도 하지 않았다(컴파일 확인만). 실제 Android 이미지의 erofs를 읽는 코드가 맞는지는 CI의 `fsck.erofs` 비교와 매핑된 부팅이 처음 보여 준다. 만드는 시간과 자리는 위의 추정이고, CI 러너의 값과 소유자 PC에서의 값은 아직 없다.
- 결정이 남은 것: 매핑하는 파티션의 dm-verity를 포기하는 맞바꿈(위 3). 그때까지 설정은 기본으로 꺼져 있다.

## R40. 쓰지 않는 메모리를 더 작은 조각으로, 더 빨리 돌려주기: 빈 페이지 보고의 남은 틈 (NFR-1, NFR-2)

**목표.** 소유자 PC(0.2.4-dev.5)에서 `dsa memory`의 "the VM holds N MiB beyond what Android uses"는 유휴 때 0-8 MiB, 부하 뒤에는 더 컸다. vm 상주는 760-1490 MiB, 게스트 사용은 687-1595 MiB였고, 앱을 닫은 뒤 메모리가 천천히 돌아왔다. 0.2.5 메모리 단계(소유자 승인)로 수십 MiB를 더 작은 조각으로, 더 빨리 돌려준다.

**방법.** 빌드하거나 실행하지 않았고 바탕 화면과 설치된 DSA는 건드리지 않았다. 고정 커널(`linux-msft-wsl-6.18.54.1`)의 `mm/page_reporting.c`, `mm/page_reporting.h`, `mm/page_alloc.c`, `mm/compaction.c`, `drivers/hv/hv_balloon.c`를 받아 읽었다(`.scratch/kernel-src/`). DSA의 패치를 차례로 적용하고 새 패치 둘을 `patch --fuzz=0`으로 원본에 적용해 결과가 같은지 확인했다. 컴파일과 시험은 CI가 한다.

**찾은 것.**

1. 보고 순서는 이미 가장 작다. Windows는 작은 백킹 페이지를 쓸 수 있는 빌드에서 0(4 KiB), 그 밖에는 9(2 MiB)이고(`page_reporting_order_for_build`), Linux(crosvm)와 macOS는 0이다. hv_balloon은 Hyper-V가 작은 범위를 거절하면 스스로 9로 돌아간다("Underlying Hyper-V does not support order less than 9"). 그래서 순서를 더 낮출 곳은 없다. 보고 한 번은 범위 최대 32개(`PAGE_REPORTING_CAPACITY`)를 hypercall 하나로 보낸다. 순서 0에서는 흩어진 4 KiB 조각마다 범위 하나이므로 범위 수가 늘지만, 묶음이 32개라 hypercall은 그 1/32이다.
2. 지연도 이미 없다. `page-reporting-no-delay.patch`가 2초 지연을 0으로 했고, 한 번에 목록의 1/16만 처리하는 예산이 끝나면 곧바로 다음 패스가 돈다. dsa-channeld는 5초마다 보고, 앱이 닫히면 기다리지 않고 회수한다(PR #724의 순서: 깨끗한 페이지 캐시, 그다음 zram). 이 순서는 그대로 두었다.
3. Hyper-V의 64 KiB 폴트 클러스터 때문에 생기는 틈이 남아 있었다. 할당자가 보고된 블록을 내주고 게스트가 그것을 쓰면 Hyper-V는 클러스터 전체(16 페이지)를 다시 들인다. 그 안의 다른 빈 블록도 다시 VM의 메모리가 되지만 게스트는 그것을 여전히 보고된 것으로 표시해 두고 다시 내놓지 않는다. 지금까지는 유휴 회수와 writeback 끝의 `rereport`만 이것을 돌려받았으므로, 앱을 쓰는 동안 이 몫이 쌓였다(R26의 "가설"). 할당 경로(`page_del_and_expand`)에서 보고된 블록이 나가는 순간을 알 수 있고, 그 블록이 클러스터보다 작으면 클러스터의 나머지 빈 블록은 버디 머리가 자기 순서에 맞춰 정렬되어 있으므로 페이지 16개를 한 번 훑으면 모두 찾는다. 나눠진 블록의 나머지(`expand`)는 원래 보고 표시가 없으므로 다음 패스가 보고한다.
4. 순서 0에서 압축 정리(compaction)는 보고에 쓸모가 없고 해롭다. 모든 빈 페이지가 그대로 보고되므로 블록을 모을 필요가 없다. 반면 압축 정리는 페이지를 빈 페이지로 복사하는데, 그 빈 페이지는 대개 호스트가 이미 돌려받은 것이라 호스트가 다시 백킹하고(클러스터째), 떠난 페이지는 그다음에야 보고된다. 유휴 회수의 `compact_memory`와 작업 집합 다듬기의 `compact_memory`, 배경 압축 정리(`compaction_proactiveness` 40)가 모두 그렇다. 앱을 닫은 뒤 VM 메모리가 한 번 오른 뒤에야 내려간 까닭의 하나로 본다(재지 않았다).
5. 게스트는 무엇을 보고했는지 내보이지 않았다. 호스트는 vm 상주와 게스트 사용의 차이만 보므로, 게스트가 아는 틈(아직 보고하지 않은 빈 메모리)과 게스트가 모르는 틈(보고했는데 호스트가 다시 들인 것)을 나눌 수 없었다.
6. macOS의 Virtualization framework 풍선(`VZVirtioTraditionalMemoryBalloonDevice`)이 빈 페이지 보고를 받는지는 확인하지 못했다. 돌려주는 길은 공통 `MemoryGrowth`의 풍선 목표다. 압박이 없는 첫 보고에서, 마지막 변경 5초 뒤 사용량 더하기 여유(사용량의 1/3, 최소 768 MiB)로 줄인다. 바꾸지 않았다. 여유를 줄이는 것은 늘리고 줄이기가 흔들리는 비용과 맞바꿈이므로 따로 정한다.

**바꾼 것.**

1. 커널 `patches/page-reporting-stats.patch`: 읽기 전용 `page_reporting.reported_kb`(지금 보고된 것으로 표시된 빈 메모리, 읽을 때 빈 목록을 센다)와 `reported_total_kb`(부팅 뒤 보고한 양). 쓰기는 `-EPERM`으로 거절한다.
2. 커널 `patches/page-reporting-taken-clusters.patch`: `page_reporting.fault_cluster_order`(기본 0, 꺼짐). 할당자가 보고된 블록을 내주고 그 블록이 클러스터보다 작으면 같은 클러스터의 빈 블록에서 보고 표시를 지우고 보고를 청한다. 그 양을 `refaulted_total_kb`가 센다. 할당 한 번에 `page_reported` 검사 하나가 더해지고(정적 키 뒤), 보고된 작은 블록이 나갈 때만 페이지 16개를 훑는다.
3. 호스트: `dsa_host_core::memory::page_reporting_cmdline`이 순서, 클러스터(있을 때), `androidboot.dsa.memory_compaction=on`(순서가 0보다 클 때만)을 명령줄에 넣는다. Windows는 작은 백킹 페이지를 쓸 때 클러스터 4를 알린다. 콘솔 머리말이 명령줄을 적기 전에 `fine_grained_memory`를 정하도록 순서를 바꿨다. Linux는 crosvm이 범위마다 돌려받으므로 클러스터가 없고, 설정한 순서가 0보다 클 때만 압축 정리를 청한다. macOS는 순서 0이라 그대로다.
4. 이미지: `dsa-channeld.rc`의 유휴 회수와 작업 집합 다듬기에서 `compact_memory`를 `ro.boot.dsa.memory_compaction=on` 조건이 붙은 따로의 동작으로 뺐다. 같은 트리거의 동작은 정의 순서대로 돌므로 순서는 "메모리를 푸는 것, (조건부) 압축 정리, `rereport`와 속성 되돌리기"다. `dsa-memory.rc`는 부팅 때 `compaction_proactiveness`를 0으로 하고, 같은 조건에서만 40으로 한다. 할당에 연속 메모리가 필요할 때의 압축 정리(kcompactd, 직접 압축)는 그대로 돈다.
5. 측정: 에이전트가 세 값을 `GuestMemory`(제어 프로토콜의 27-29번)로 보내고, `memory.cil`이 그 세 파일을 에이전트만 읽게 한다. 세션이 같은 부팅의 두 보고에서 분당 보고량을 낸다. `dsa memory`는 "free page reporting: N of M MiB free is back on the host, K MiB not yet; T MiB reported since boot (R MiB a minute since the report before); F MiB taken back in with a neighbour and reported again"을 적고, `--json`은 `guest.page_reporting`에, 서비스 로그의 메모리 줄은 끝에 같은 것을 적는다. CI 메모리 측정의 두 표에도 "guest free"와 "guest free handed back to the host"를 더했다.

**기대하는 절감(재지 않았다).** 앱을 쓰는 동안 클러스터로 다시 들어온 빈 페이지: R26이 vm private와 게스트 사용의 차이로 본 130-310 MiB 가운데 이 몫이 수십 MiB라면 그것이 앱을 쓰는 동안에도 돌아간다. 앱을 닫은 뒤의 압축 정리 한 번이 옮기던 페이지(게스트 사용의 수백 MiB)를 호스트가 잠깐 다시 백킹하던 봉우리와 그 시간이 없어진다. 배경 압축 정리가 쓰던 CPU도 없어진다. 비용은 클러스터를 다시 보고하는 hypercall(보고된 작은 블록이 나갈 때마다 최대 범위 15개)과, 할당에 큰 연속 블록이 필요할 때 압축 정리를 그때 하는 지연이다.

**확인하지 못한 것(PC에서 잴 것).**

- 위 수치 모두. 같은 이미지, 같은 앱으로 바꾸기 전(0.2.4-dev.5)과 뒤를 견준다. `dsa memory`를 앱을 연 채, 닫은 직후, 1분 뒤, 5분 뒤에 읽어 vm 상주와 private, "beyond what Android uses", 새 "free page reporting" 줄의 "not yet"과 "taken back in with a neighbour", 분당 보고량을 적는다. 앱을 닫은 뒤 vm 상주가 오르지 않고 바로 내려가는지, "beyond"가 부하 뒤에도 작게 남는지 본다.
- `refaulted_total_kb`가 실제로 자라는지. 자라지 않으면 Hyper-V가 클러스터 안의 이웃을 들이지 않는다는 뜻이므로, 그때는 패치 2의 효과가 없고 R26의 가설도 틀린 것이다.
- `reported_kb`를 읽는 비용. 빈 블록이 많을 때 빈 목록 전체를 영역 잠금 아래에서 센다. 에이전트는 5분마다와 `dsa memory` 때만 읽는다.
- 압축 정리를 끈 뒤 GPU, 네트워크처럼 연속 메모리를 쓰는 할당이 느려지지 않는지(커널 로그의 할당 실패, `/proc/vmstat`의 `compact_stall`).
- Kernel test가 두 패치를 컴파일하는지, Image test가 SELinux enforcing에서 에이전트가 세 파일을 읽게 하는지(부팅 로그의 `avc`, `dsa memory`가 "does not say what it reported"를 적지 않는지).
