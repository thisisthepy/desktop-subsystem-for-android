# 제거

## Windows

Desktop Subsystem for Android™(DSA)는 다음 중 한 방법으로 제거합니다.

- Windows **설정** > **앱** > **설치된 앱**에서 Desktop Subsystem for Android를 찾아 **제거**를
  선택합니다.
- 설치 프로그램을 다시 실행하고 **Uninstall**을 누릅니다.

제거하면 로그인 여부와 관계없이 PC의 모든 사용자에 대해 다음을 지웁니다.

- 프로그램, `DsaHost` 서비스, `dsa` 명령, DSA 설정, APK 설치 관리자
- 내려받은 Android 이미지와 업데이트
- Android 앱의 시작 메뉴 항목과 **설치된 앱** 항목
- 파일 탐색기의 **Android** 항목, `.apk` 파일 연결, 바로 가기 메뉴 항목
- DSA의 설정과 로그

사용자마다 있는 Android 디스크 `%LOCALAPPDATA%\DSA\android.vhdx`는 **남깁니다**. 그래서 나중에 다시
설치하면 앱과 데이터를 그대로 찾습니다. 데이터까지 지우려면 제거하기 전에 `dsa reset-data --yes`를
실행하거나, 제거한 뒤 이 파일을 지웁니다.

폴더 리디렉션으로 시작 메뉴 위치를 옮긴 사용자는 Android 앱 항목이 남습니다. 제거 프로그램은 기본 위치만
찾기 때문입니다. 남은 항목은 직접 지우십시오.

DSA는 WSL 설정을 바꾸지 않으므로 WSL은 그대로입니다.

## Linux (미출시)

```
sudo apt remove desktop-subsystem-for-android    # Debian, Ubuntu
sudo dnf remove desktop-subsystem-for-android    # Fedora
```

패키지를 제거하면 패키지가 등록한 저장소 항목도 함께 지웁니다.

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. DSA는 Google, Microsoft와
제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
