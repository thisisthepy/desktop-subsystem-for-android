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
5. VM 크기: 이제 기본 최대 크기는 호스트 메모리에서 Windows 몫(10%, 최소 2 GiB)을 뺀 값이다. 대가가 있다. 커널은 모든 메모리의 struct page를 부팅 때 만들고 채우므로, 최대 크기의 약 1.56%가 늘 상주한다. 32 GiB PC(VM 29.5 GiB)에서는 약 460 MiB로, 3 GiB일 때의 48 MiB보다 400 MiB 이상 많다. 이것은 NFR-2의 "대기 메모리가 눈에 띄게 늘지 않는다"와 충돌할 수 있으므로 실측해야 한다. 피하는 방법은 메모리를 작게 시작해 필요할 때 늘리는 것(Hyper-V의 메모리 핫애드, `hv_balloon`)인데, HCS로 만든 VM에서 쓸 수 있는지는 확인하지 못했다. 실측에서 늘어나면 사용자가 `dsa settings set memory-limit 8G`처럼 상한을 둘 수 있다.

**WSL과의 비교 (공개 자료, 측정 아님).** WSL2는 기본으로 호스트 메모리의 50%를 VM 최대 크기로 두고, 같은 struct page 비용을 낸다. 배포판 하나가 쉬고 있을 때 `vmmemWSL`은 공개된 사용자 보고에서 수백 MiB 수준이다. WSL은 `autoMemoryReclaim`(gradual은 cgroup 회수, dropcache는 캐시 버리기)과 빈 페이지 보고로 쓰지 않는 메모리를 돌려준다. DSA도 같은 장치(지연 커밋, 빈 페이지 보고, 차가운 페이지 힌트, 작은 백킹 페이지)를 쓴다. 차이는 안드로이드가 쉬는 동안에도 system_server와 앱 프로세스를 띄워 두는 데 있다. 그래서 목표는 프로세스 수를 줄이고 닫은 앱을 바로 끝내는 쪽이다.

**목표 (실측 전).** Settings 하나를 연 상태의 DSA 호스트 private 합 단기 400 MiB, 최종 250 MiB. 이번 변경 뒤 기대치는 dsa-wm private -20~-60 MiB, 게스트 프로세스 -25~-40 MiB, 닫은 앱마다 -30~-100 MiB(몇 초 안에)다.

**PC에서 잴 것.** `measure.ps1 -Part open`으로 Settings 하나를 연 상태의 각 프로세스 private와 `dsa memory`의 커널 내역을 기록한다. `-Part close`로 앱을 닫은 뒤 vm 상주 메모리가 몇 초 안에 얼마나 줄었는지 잰다. 32 GiB PC에서 `dsa settings set memory-limit 3G`와 `auto`를 바꿔 가며 `Dozing` 상주 메모리와 부팅 시간을 비교한다. `dsa-wm.log`에서 "no task window is shown" 줄의 GPU 메모리를 확인한다.

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
