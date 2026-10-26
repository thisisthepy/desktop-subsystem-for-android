# Desktop Subsystem for Android™

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/LICENSE)

[English](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/README.md) · **한국어**

**안드로이드 앱을 진짜 데스크톱 창으로, 오픈 소스로.**

*안드로이드는 전용 경량 VM에서 돕니다. 앱마다 자기 창, 시작 메뉴 항목, 알림을 가집니다.*

> DSA는 독립 오픈 소스 프로젝트입니다. Microsoft나 Google과 제휴하거나 그들의 보증, 후원을 받지 않습니다. Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다.

---

## 상태

**설계 단계입니다. 아직 실행할 코드는 없습니다.** 저장소에는 아키텍처와 요구사항이 있고, 요구사항이 합의되면 구현을 시작합니다. 첫 호스트는 Windows 11입니다. Linux와 macOS 호스트는 계획 단계이며 시작하지 않았습니다.

## 왜 만드는가

Microsoft는 2025년 3월 5일에 Windows Subsystem for Android 지원을 종료했습니다. 핵심 부분이 닫힌 바이너리라서 Microsoft 밖에서는 이어갈 수 없습니다. DSA는 같은 수준의 창 통합을 오픈 소스로 만들고, 백그라운드에 상주하기에 더 가볍게 만드는 것을 목표로 합니다.

## 동작 방식 (계획)

| 부분 | 방식 | 상태 |
|---|---|---|
| 안드로이드 | WSL과 같은 Windows API로 만든 전용 경량 VM에서 돌고, 바로 안드로이드로 부팅합니다. 사용자의 WSL 설정은 건드리지 않습니다 | 계획 |
| 커널 | DSA가 binder를 내장한 VM 커널을 함께 배포합니다. 사용자는 커널을 빌드하지 않습니다 | 계획 |
| Windows | 커스텀 hwcomposer가 안드로이드 태스크를 DSA 창 관리자에 넘기고, 창 관리자는 그 태스크를 앱마다 따로 있는 작은 프로그램의 Win32 창에 보여 줍니다. 이 프로그램은 앱의 이름과 아이콘을 가지므로 작업 표시줄과 작업 관리자에 앱이 제 모습으로 보입니다. WSLg와 RDP는 쓰지 않습니다 | 계획 |
| 그래픽 | 모든 앱의 OpenGL ES는 ANGLE로 Vulkan 위에서 돌고, Vulkan은 원격 방식으로 호스트 GPU에서 실행합니다(gfxstream에서 출발) | 계획 |
| 장치 | 동영상 코덱, 오디오, 전원은 HAL 서비스로 호스트에 옮깁니다. 안드로이드 프레임워크는 게스트에 남습니다 | 계획 |
| 셸 | 안드로이드 런처, 상태바, 키보드 대신 시작 메뉴, 알림 센터, Windows IME가 그 역할을 합니다 | 계획 |
| 대기 | 안드로이드의 Doze를 대기 상태로 써서, CPU는 쉬면서 알림과 위젯은 계속 동작합니다 | 계획 |
| 위젯 | 안드로이드 위젯을 Windows 바탕화면에 띄웁니다 | 계획 |

```
[안드로이드 VM]                                  [Windows 호스트: DSA 서비스]
 앱 → SurfaceFlinger
   → hwcomposer ─── 창 제어, 입력 ─────────────── 창 관리자 → 앱별 프로그램: 태스크마다 창
   → ANGLE → Vulkan ─── Vulkan 원격 ────────────→ 호스트 Vulkan
 에이전트 ─── 앱 목록, 알림, Intent, IME ─────────→ 시작 메뉴, 알림, TSF
 HAL 클라이언트 ─── RPC Binder ────────────────→ 호스트 코덱, 오디오, 전원
```

## DSA가 싣지 않는 것

- **Google Play 서비스와 앱 스토어.** 라이선스 때문에 프로젝트가 함께 배포할 수 없고, 사용자가 고르는 선택지로 남깁니다.
- **Microsoft의 WSA 바이너리.** DSA는 그것을 다시 쓰거나 재배포하지 않습니다.

## 저장소

- `dsa/`: 모든 코드 (합의된 첫 요구사항을 구현하기 전까지 비어 있습니다)
- `docs/locales/`: 이 README의 번역
- `scripts/tests/`: 저장소 검사

작업은 `develop` 브랜치에서 합니다. `main`은 거기서 PR을 거쳐 공개됩니다.

## 라이선스

[Apache-2.0](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/LICENSE)
