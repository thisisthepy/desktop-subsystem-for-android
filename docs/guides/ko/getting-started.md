# 시작하기

Desktop Subsystem for Android™(DSA)는 자체 커널을 갖춘 가벼운 가상 머신에서 Android를 실행합니다.
Android 앱은 하나하나 데스크톱의 독립된 창으로 열립니다.

## 플랫폼별 현황

| 호스트 | 현황 |
|---|---|
| Windows 11, x64 | 미리 보기 버전을 출시했습니다. 기능은 CI에서 테스트하며, 실제 하드웨어에서 직접 확인하는 작업은 진행 중입니다. |
| Windows 11, ARM64 | 진행 중입니다. CI에서 빌드와 설치까지 확인하지만 아직 출시하지 않았습니다. |
| Linux, x64와 ARM64 | 진행 중입니다. `.deb`, `.rpm` 패키지를 CI에서 빌드하지만 아직 출시하지 않았습니다. |
| macOS, Apple silicon | 진행 중입니다. `DSA.app`과 `.pkg`, `.dmg`를 CI에서 빌드하지만 아직 출시하지 않았습니다. |

## Windows에 설치하기

x64 PC의 64비트 Windows 11이 필요하고, Windows 기능 **가상 머신 플랫폼**이 켜져 있어야 합니다.

1. 가상 머신 플랫폼을 켭니다. **Windows 기능 켜기/끄기**를 열고 **가상 머신 플랫폼**을 선택한 다음
   **확인**을 누르고 PC를 다시 시작합니다. 설치 프로그램은 이 기능을 대신 켜 주지 않으며, 꺼져 있으면 안내
   메시지를 보여 주고 멈춥니다.
2. [최신 릴리스](https://github.com/thisisthepy/desktop-subsystem-for-android/releases/latest)에서
   `DSA-v<version>-win-x64-setup.exe`와 `SHA256SUMS.txt`를 내려받습니다.
3. 내려받은 파일을 확인합니다. 다음 명령이 출력하는 해시가 `SHA256SUMS.txt`에 적힌 값과 같아야 합니다.

   ```
   certutil -hashfile DSA-v<version>-win-x64-setup.exe SHA256
   ```

4. 설치 프로그램을 실행하고 **Install**을 누른 다음 관리자 권한 요청을 승인합니다. 설치 프로그램은 영어로만
   표시됩니다.
5. 첫 Android 이미지를 기다립니다. 설치 파일에 이미지가 없으면 DSA 서비스가 백그라운드에서 내려받습니다.
   진행 상황은 `dsa status`로 볼 수 있습니다.
6. 시작 메뉴에서 앱을 열거나, 터미널에서 확인합니다.

   ```
   dsa start --wait
   dsa launch com.android.settings
   ```

## 설치되는 것

- `DsaHost` 서비스: 사용자마다 Android를 실행합니다.
- `dsa` 명령
- 시작 메뉴의 **DSA Settings**(DSA 설정 앱)
- 파일 탐색기 탐색 창의 **Android**(Android 파일 열기)
- APK 설치 관리자: `.apk` 파일을 엽니다.
- DSA 창 관리자(`dsa-wm`): 로그인할 때 시작하면서 Android도 함께 시작합니다. 그래서 처음 여는 앱이
  Android 부팅을 기다리지 않습니다.

Windows 사용자마다 자기 Android와 데이터 디스크(`%LOCALAPPDATA%\DSA\android.vhdx`)를 따로 가집니다.
다른 사용자는 이 디스크를 읽을 수 없습니다.

DSA는 자체 커널과 자체 가상 머신을 씁니다. WSL 설정, 커널, 배포판은 쓰지도 바꾸지도 않습니다.

## 업데이트

DSA는 프로그램과 Android 이미지를 스스로 업데이트하며, 둘 다 서명되어 있습니다. Android 이미지가 부팅에
실패하면 이전 이미지로 돌아가고, `dsa status`가 이를 알려 줍니다. 관리자는 DSA 설정의 **업데이트**
페이지에서 자동 업데이트를 끌 수 있습니다.

## Linux (진행 중, 미출시)

Linux용 DSA는 `.deb`(Debian, Ubuntu)와 `.rpm`(Fedora, openSUSE)으로 빌드합니다. KVM(`/dev/kvm`)과
Wayland 데스크톱이 필요하며, Android는 crosvm 가상 머신에서 실행됩니다.

패키지를 설치하면 DSA의 서명된 패키지 저장소도 함께 등록됩니다. 이후 버전은 `apt`, `dnf`, `zypper`나
소프트웨어 센터로 받습니다. Linux에서는 DSA 자체 업데이터가 없습니다.

- Debian, Ubuntu: `/etc/apt/sources.list.d/dsa.sources`, 키는 `/usr/share/keyrings/dsa-archive-keyring.gpg`
- Fedora, openSUSE: `/etc/yum.repos.d/dsa.repo`(zypper가 있으면 `/etc/zypp/repos.d/dsa.repo`도),
  키는 `/etc/pki/rpm-gpg/RPM-GPG-KEY-dsa`

저장소는 `https://thisisthepy.github.io/desktop-subsystem-for-android/`에서 제공할 예정이며, 아직
공개하지 않았습니다.

출시되면 첫 설치는 다음과 같습니다.

```
sudo apt install ./desktop-subsystem-for-android_<version>_amd64.deb   # Debian, Ubuntu
sudo dnf install ./desktop-subsystem-for-android-<version>*.rpm       # Fedora
```

패키지는 `dsa-hostd` 시스템 서비스, `dsa-devices`와 `dsa-compositor` 사용자 서비스, `/usr/bin`의
`dsa` 명령, **DSA 파일** 메뉴 항목, 파일 동작 **Android 앱에서 열기**, **Android 앱과 공유**,
**Android에 설치**를 설치합니다.

## macOS (진행 중, 미출시)

macOS용 DSA는 Apple silicon에서 Apple의 Virtualization 프레임워크로 Android를 실행합니다. `DSA.app`을
설치 패키지(`DSA-<version>.pkg`)와 디스크 이미지(`DSA-<version>.dmg`)로 빌드합니다. CI 빌드는 임시(ad
hoc) 서명만 되어 있어 빌드한 컴퓨터에서만 실행됩니다. Developer ID로 서명한 릴리스는 아직 없습니다.

## 다음 단계

- [앱과 창](apps.md)
- [APK 설치](install-apks.md)

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. macOS는 Apple Inc.의
상표입니다. DSA는 Google, Microsoft, Apple과 제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
