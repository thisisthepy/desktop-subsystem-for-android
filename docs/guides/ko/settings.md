# 설정

**DSA 설정**은 Windows용 Desktop Subsystem for Android™(DSA)의 설정 앱입니다. 시작 메뉴에서
**DSA Settings**를 선택해 엽니다. Windows 표시 언어에 따라 한국어나 영어로 표시되고, Windows 테마를
따릅니다.

DSA 설정에서 바꾸는 내용은 모두 `dsa` 명령을 거칩니다. 그래서 명령줄에서도 같은 값을 보고 바꿀 수 있습니다.

```
dsa settings                        # 모든 설정과 값
dsa settings set <name> <value>     # 하나를 바꿉니다. 스위치는 on이나 off를 받습니다
```

일부 설정은 PC의 모든 사용자에게 적용되므로 관리자만 바꿀 수 있습니다. DSA 설정은 이런 설정에 방패 아이콘을
표시하고, 바꿀 때 관리자 권한을 요청합니다. 아래 표에는 "(관리자)"로 표시했습니다.

## 페이지

| 페이지 | 내용 | 명령줄 |
|---|---|---|
| **일반** | Android 상태, **시작**, **종료**, **지금 안드로이드 일시 정지**, 마지막 문제, **내 로그 폴더 열기** | `dsa status`, `dsa start`, `dsa stop`, `dsa freeze` |
| **메모리** | **메모리 한도**, Android 가상 머신의 크기, 부분별 사용량 | `memory-limit`, `dsa memory` |
| **앱** | 설치된 앱: **열기**, **앱 정보**, **제거**, **바탕 화면 바로 가기 만들기**, **크기 조절**, 창이 열리는 크기 | `dsa apps`, `dsa app-window` |
| **백그라운드에서 실행 중** | 창 없이 실행 중인 앱: **열기**, **중지**, 중지 전에 물을지 여부 | `dsa background`, `stop-without-asking` |
| **위젯** | 바탕 화면의 위젯, **바탕 화면에 추가** | `dsa widgets` |
| **디스플레이 및 GPU** | **렌더링 GPU**, **창 관리자 다시 시작**, **DSA 앱 창에서 Windows 터치 제스처 끄기**(Windows 11만, 기본값은 켬) | `render-gpu`, `dsa gpus`, `touch-gestures-off` |
| **기기 정보** | 먼저 바꿀 수 없는 이 컴퓨터의 정보: 이름, 제조사와 모델, **복사**가 있는 **기기 사양**. 그다음 Android가 앱에 알리는 제조사, 모델, 빌드 지문 | `dsa settings device`, `device.<field>` |
| **네트워크** | Android의 네트워크 연결 방식, 주소, 이름을 답하는 곳(DNS), 게이트웨이, **고급 네트워킹**, **개발자 모드**(adb) | `dsa network`, `advanced-networking`, `developer-mode`, `adb-port`, `adb-auto-connect` |
| **업데이트** | **Android**: 실행 중인 버전과 빌드, 따르는 버전, 진행률과 **지금 확인**(관리자)이 있는 Android 업데이트, **안드로이드 이미지 자동 업데이트**(관리자), **사용할 버전**(관리자). **DSA**: **DSA 자동 업데이트**(관리자), **프리릴리스 버전 받기**(관리자), **지금 설치** | `dsa android version`, `dsa android use`, `image-updates`, `program-updates`, `prerelease-updates`, `dsa update` |
| **고급** | 클립보드, 파일, 웹 링크, 유휴 상태 일시 정지, 초기화, ARM 앱, 추적(관리자), 로그 폴더 | 아래 표 |
| **로그** | Android 실시간 로그, **진단 정보 저장** | `dsa logcat`, `dsa logs collect` |
| **정보** | 버전, 라이선스, 소스 코드 | |

## 고급 페이지

| 설정 | 기본값 | 명령줄 |
|---|---|---|
| **안드로이드와 클립보드 공유** | 켬 | `clipboard` |
| **Windows 폴더를 안드로이드와 공유** | 켬 | `file-sharing` |
| **공유할 폴더** | 여섯 폴더 모두 | `shared-folders` |
| **유휴 상태에서 안드로이드 일시 정지** | 끔 | `freeze` |
| **안드로이드 앱에서 웹 링크 열기** | 끔 | `links-in-android` |
| **안드로이드 데이터 초기화** | | `dsa reset-data` |
| **ARM 전용 앱 실행** | | `arm-translation` |
| **Google의 ARM 번역기** (ARM64 제외) | | `dsa arm-translation fetch google` |
| **기본 번역기 사용** (ARM64 제외) | | `dsa arm-translation use-default` |
| **입력 추적**(관리자) | 끔 | `input-trace` |
| **크기 조정 추적**(관리자) | 끔 | `resize-trace` |

## 바꾼 설정이 적용되는 때

- **렌더링 GPU**: 창 관리자가 다시 시작할 때 적용됩니다. 다음에 로그인하거나 **창 관리자 다시 시작**을 누르면
  됩니다.
- **메모리 한도**, **기기 정보**, **고급 네트워킹**, **ARM 전용 앱 실행**, Android 버전: 다음에 Android를
  시작할 때 적용됩니다. Android를 다시 시작하려면 **일반** 페이지에서 종료한 다음 앱을 엽니다.
- 나머지: 바로 적용됩니다.

## Android가 이름을 찾는 방법

Android 앱이 찾는 이름은 모두 컴퓨터가 답합니다. Windows 앱이 쓰는 것과 같은 해석기를 씁니다(Linux와
macOS에서는 시스템 해석기). 그래서 hosts 파일, 회사의 이름 규칙, VPN의 DNS 서버, DNS 필터가 컴퓨터에서와
같이 Android 앱에도 적용됩니다. Android가 직접 DNS 서버에 묻지 않습니다.

- **고급 네트워킹**을 켜도 같습니다. 컴퓨터가 같은 네트워크에 있어 그 이름을 알고, 컴퓨터의 VPN이나 DNS
  필터도 그대로 적용됩니다.
- Android의 **비공개 DNS**는 기본으로 꺼져 있습니다. 켜면 컴퓨터를 거치지 않고 DNS 서버에 묻기 때문입니다.
  Android 설정에서 비공개 DNS 서버를 고르면 Android는 그 서버를 씁니다.
- DSA가 한동안 답하지 못하면, 다시 답할 수 있을 때까지 Android가 네트워크의 DNS 서버에 묻습니다.
- 지금 누가 답하는지는 **네트워크** 페이지의 **DNS** 줄과 `dsa network`에 나옵니다. 컴퓨터의 해석기인지,
  아니면 네트워크의 DNS 서버인지(그 목록과 함께) 알려 줍니다.

## 명령줄에서 쓰는 값

- `render-gpu`: `display`(주 모니터의 GPU, 기본값), `integrated`, `discrete`, 또는 `dsa gpus`가 보여 주는
  GPU 이름의 일부
- `memory-limit`: `auto`(기본값. 고정된 한도가 없고, PC의 메모리에서 Windows 몫을 뺀 만큼까지 늘어납니다), 또는 `4g`, `3072`(MiB)
  같은 크기
- `shared-folders`: `all`, `none`, 또는 쉼표로 구분한 폴더 이름(예: `Documents,Pictures`)
- `adb-port`: 1부터 65535까지의 TCP 포트. 기본값은 58526입니다.
- `touch-gestures-off`: `on`(기본값)이면 DSA 앱 창이 앞에 있는 동안 Windows 11의 세 손가락, 네 손가락 터치
  제스처를 꺼서 앱이 열 손가락까지 받게 하고, 다른 창이 앞에 오면 원래 설정으로 되돌립니다. Windows가 거부하면
  이 설정을 쓸 수 없고 설정 앱이 이유를 보여 줍니다. 그래도 Windows 설정의 Bluetooth 및 장치, 터치에서 모든 앱에
  대해 제스처를 끌 수 있습니다.
- `prerelease-updates`: `on`이면 정식 버전과 함께 프리릴리스도 받고, `off`이면 정식 버전만 받습니다. 처음 값은
  설치한 설치 파일을 따릅니다. 프리릴리스 설치 파일은 켜고, 정식 설치 파일은 끈 채로 두며, 업그레이드는 고른 값을
  그대로 둡니다. 1.0 전에는 모든 릴리스가 프리릴리스입니다. 꺼도 DSA가 이전 버전으로 돌아가지 않으며, 설치된
  버전보다 새 정식 버전이 나올 때까지 기다립니다.
- `device.<field>`: `manufacturer`, `brand`, `model`, `device`, `name`, `characteristics`,
  `fingerprint`, 또는 `fingerprint.release` 같은 지문의 일부. `dsa settings reset device`는 모든 값을
  되돌립니다.

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. DSA는 Google, Microsoft와
제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
