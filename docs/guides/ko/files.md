# 파일

Desktop Subsystem for Android™(DSA)에서는 파일 탐색기로 Android 파일을 열고, Windows 폴더를 Android 앱과
공유하고, 파일을 Android 앱으로 보낼 수 있습니다. 이 페이지는 Windows용 DSA를 설명합니다.

Windows 탐색기와 시작 메뉴에 DSA가 추가하는 항목은 영어로만 표시됩니다. 아래에는 화면에 보이는 그대로
적었습니다.

## 파일 탐색기에서 Android 파일 열기

다음 중 하나를 씁니다. 모두 Android가 실행 중이 아니면 먼저 시작합니다.

- 파일 탐색기 탐색 창의 **Android**: WSL의 **Linux** 바로 아래에 있습니다.
- DSA 설정 **고급** 페이지의 **안드로이드 파일**
- `dsa files open`: `dsa files`는 열지 않고 저장소 위치만 알려 줍니다.

Android 앱이 저장한 사진, 다운로드 같은 파일이 있는 Android 공유 저장소가 열립니다. 일반 폴더처럼 파일을
복사해 넣거나 꺼낼 수 있습니다. 파일 탐색기는 Windows의 WebDAV 클라이언트로 저장소에 연결합니다. 저장소가
응답하지 않으면 DSA가 이유와 함께 오류를 보여 줍니다.

## Windows 폴더를 Android 앱과 공유하기

문서, 다운로드, 사진, 음악, 동영상, 바탕 화면 폴더는 Android 공유 저장소의 `Host` 아래(예:
`Host/Pictures`)에 나타납니다. Android 앱은 여기서 파일을 열고 저장할 수 있습니다. 앱에 Android 저장소
권한이 있어야 하며, Windows는 내 계정이 할 수 있는 일만 허용합니다.

공유는 기본으로 켜져 있습니다. 바꾸려면 DSA 설정 **고급** 페이지를 엽니다.

- **Windows 폴더를 안드로이드와 공유**를 끄면 어떤 폴더도 공유하지 않습니다.
- **공유할 폴더**에서 폴더 선택을 해제하면 그 폴더는 곧바로 Android에서 사라집니다.

명령줄에서는 다음과 같이 합니다.

```
dsa settings set shared-folders Documents,Pictures   # 이 폴더만 공유합니다. all, none도 됩니다
dsa settings set file-sharing off                     # 어떤 폴더도 공유하지 않습니다
```

이전 버전은 `Host` 대신 `Windows`라는 폴더를 썼습니다. Android는 예전 폴더가 비면 지우므로, 거기 저장한
파일은 그대로 남습니다.

## Android 앱으로 파일 보내기

- **끌어서 놓기**: 파일 탐색기에서 파일을 앱 창으로 끌어다 놓으면 그 앱으로 갑니다. APK는 APK 설치 관리자로
  갑니다. 폴더는 보낼 수 없습니다.
- **바로 가기 메뉴**: 파일을 마우스 오른쪽 단추로 누르고 **Open in an Android app**(Android 앱에서
  열기)이나 **Share with an Android app**(Android 앱과 공유)을 선택합니다. Windows 11에서는 **추가 옵션
  표시** 아래에 있습니다. 그러면 Android가 어느 앱으로 받을지 묻습니다.
- **명령줄**: `dsa open <file>`, `dsa share <file>`

이 방법으로는 64 MiB까지 보낼 수 있습니다. 더 큰 파일은 보내기 전에 거부됩니다. 큰 파일은 공유 폴더에
넣으십시오.

## Android에서 파일 가져오기

- Android에서 `Host/<folder>` 아래에 저장하면 PC의 해당 폴더에 바로 생깁니다.
- 또는 파일 탐색기에서 **Android**를 열고 파일을 복사해 꺼냅니다.

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. DSA는 Google, Microsoft와
제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
