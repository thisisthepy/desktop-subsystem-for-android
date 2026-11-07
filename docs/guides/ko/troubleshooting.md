# 문제 해결과 로그

Desktop Subsystem for Android™(DSA)가 제대로 동작하지 않는 이유를 찾고, Android 로그를 보고, 문제를
보고하는 방법을 설명합니다. Windows를 기준으로 하며, Linux는 따로 표시했습니다.

## 먼저 상태 확인하기

```
dsa status          # Android 상태, 문제, 대기 중인 업데이트
dsa start --wait    # Android를 시작하고 부팅이 끝날 때까지 기다립니다
```

DSA 설정 **일반** 페이지에도 같은 상태가 나옵니다.

| `dsa status` | DSA 설정 | 뜻 |
|---|---|---|
| `Active` | **실행 중** | Android 부팅이 끝났고 그 안의 DSA 에이전트가 연결되었습니다 |
| `Dozing` | **대기(Doze)** | Android가 유휴 상태입니다. [메모리](memory.md)를 참고하십시오 |
| `Frozen` | **일시 정지됨** | 창을 열 때까지 Android가 멈춰 있습니다 |
| `Stalled` | **준비되지 않음** | Android가 실행 중이지만 너무 오래 걸리고 있습니다. 다음 줄에 이유와 할 일이 나옵니다 |

## 자주 생기는 문제

| 문제 | 할 일 |
|---|---|
| 설치 프로그램이 가상 머신 플랫폼이 꺼져 있다고 합니다 | Windows 기능을 켜고 PC를 다시 시작한 뒤 설치 프로그램을 다시 실행합니다. [시작하기](getting-started.md)를 참고하십시오 |
| 앱 창이 열리지 않습니다 | 로그아웃했다가 다시 로그인합니다. 창 관리자는 로그인할 때 시작합니다 |
| 앱이 이상한 크기로 열립니다 | `dsa app-window <package> reset` |
| 이미지 업데이트 뒤 Android가 부팅하지 않습니다 | 할 일은 없습니다. DSA가 스스로 이전 이미지로 되돌리고 `dsa status`가 알려 줍니다 |
| Android에서 네트워크가 안 됩니다 | DSA 설정 **네트워크** 페이지를 열거나 `dsa network`를 실행합니다. 연결 방식, 주소, DNS 서버, 문제가 나옵니다 |
| Android가 고장 났고 다른 방법이 없습니다 | 아래처럼 Android 데이터를 초기화합니다 |

### Android 데이터 초기화

초기화하면 설치한 앱, 계정, 모든 앱 데이터가 지워지고, Android는 처음 설치했을 때 상태로 시작합니다.
Android 이미지와 DSA 설정은 남습니다.

- DSA 설정: **고급** 페이지 > **안드로이드 데이터 초기화** > **초기화**
- 명령줄: `dsa reset-data`는 무엇이 지워지는지 알려 줍니다. `dsa reset-data --yes`는 실제로 지웁니다.

## Android 로그 보기

Android 로그는 Android 안의 메모리에만 있고 파일로 저장되지 않습니다. 무언가 로그를 읽는 동안에만 PC로
전달됩니다.

- DSA 설정: **Android 로그** 페이지. 태그나 내용으로 거르고, 일시 정지할 수 있습니다.
- 명령줄:

  ```
  dsa logcat                        # 로그를 계속 봅니다. Ctrl+C로 끝냅니다
  dsa logcat -d -b all *:W          # 지금 쌓인 로그 중 경고 이상
  dsa logcat ActivityManager:I *:S  # 태그 하나만
  ```

## 문제 보고하기

1. 진단 정보를 저장합니다. **Android 로그** 페이지에서 **진단 정보 저장**을 누르거나 다음 명령을
   실행합니다.

   ```
   dsa logs collect -o diag.zip
   ```

   1분 이상 걸립니다.
2. zip 파일을 [이슈](https://github.com/thisisthepy/desktop-subsystem-for-android/issues)에 첨부합니다.

압축 파일에는 Android의 전체 로그, 충돌 기록(tombstone, ANR 추적, dropbox, pstore), Android 시스템 서비스
요약, 이 PC에서 나와 관련된 DSA 로그가 들어갑니다. 다른 사용자의 기록은 빠집니다. 압축 파일 안의
`summary.txt`에 모으지 못한 항목이 적혀 있습니다.

## Windows의 로그 위치

| 폴더 | 파일 |
|---|---|
| `%LOCALAPPDATA%\DSA\logs\` | `dsa-wm.log`(창 관리자), `dsa-app-window.log`(앱 창) |
| `%ProgramData%\DSA\logs\` | `dsa-hostd.log`(서비스), `android-<SID>.log`(사용자별 Android 콘솔, 부팅마다 최대 2 MiB), `setup-<version>.log`(자체 업데이트) |

DSA 설정 **고급** 페이지의 **내 로그 폴더 열기**와 **서비스 로그 폴더 열기(관리자)**로 이 폴더를 엽니다.

## Linux의 로그 위치 (미출시)

서비스는 systemd 저널에 기록합니다.

```
journalctl -u dsa-hostd
journalctl --user -u dsa-devices -u dsa-compositor
```

## 모든 명령

인수 없이 `dsa`를 실행하면 모든 명령이 나옵니다. `--json`을 붙이면 프로그램이 읽을 수 있는 형식으로
출력합니다.

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. DSA는 Google, Microsoft와
제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
