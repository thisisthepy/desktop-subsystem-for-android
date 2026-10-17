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
