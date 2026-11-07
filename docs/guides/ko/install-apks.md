# APK 설치

Desktop Subsystem for Android™(DSA)는 앱 스토어를 제공하지 않습니다. 앱은 APK 파일로 설치하며, APK 설치
관리자나 `dsa` 명령을 씁니다. 이 페이지는 Windows용 DSA를 설명합니다.

## APK 설치 관리자로 설치하기

1. 파일 탐색기에서 `.apk` 파일을 두 번 클릭합니다. **APK 설치 관리자**가 열립니다.
2. APK 정보를 확인합니다. Android를 시작하지 않고도 다음 내용을 보여 줍니다.
   - 앱 이름, 아이콘, 패키지, 버전
   - 게시자: APK에는 게시자 이름이 없으므로 서명 인증서에서 가져옵니다.
   - 출처: 파일을 내려받은 호스트나 파일이 있는 폴더
   - **Capabilities**: 앱이 요청하는 처음 몇 개의 권한. **More**를 누르면 모든 권한과 대상 Android 버전,
     크기, 프로세서 종류, 인증서의 SHA-256을 보여 줍니다.

   APK에 다른 프로세서용 코드만 있으면 경고가 표시됩니다.
3. **설치**를 누릅니다. 이미 설치된 앱이면 단추가 **업데이트**나 **다시 설치**로 바뀝니다. Android가 실행
   중이 아니면 먼저 시작합니다.
4. **준비되면 실행**이 선택되어 있으면(기본값) 설치가 끝난 뒤 앱이 열리고 설치 관리자가 닫힙니다. 선택하지
   않았으면 **실행**을 누르거나, 나중에 시작 메뉴에서 앱을 엽니다.

Android가 실행 중이면, 새로 설치하는지, 업데이트인지, 같은 버전을 다시 설치하는지도 알려 줍니다. Android가
거부할 경우(더 오래된 버전이거나, 같은 앱이 다른 키로 서명된 경우)에는 그 이유를 보여 줍니다.

APK 파일을 APK 설치 관리자, DSA 설정, 앱 창에 끌어다 놓아도 됩니다. APK마다 설치 관리자 창이 따로 열립니다.

## 명령줄로 설치하기

```
dsa apk-info app.apk            # APK 정보를 봅니다. Android를 시작하지 않습니다
dsa install app.apk             # 설치합니다. Android가 꺼져 있으면 먼저 시작합니다
dsa install app.apk --progress  # 단계마다 진행 상황도 출력합니다
```

Android가 APK를 거부하면 `dsa install`은 종료 코드 1로 끝나고, 메시지는 `not installed (<reason>):`로
시작합니다.

## 설치가 거부되는 이유

| 이유 | 뜻 | 할 일 |
|---|---|---|
| `incompatible` | 다른 프로세서, 더 새로운 Android 버전, 또는 이 기기에 없는 기능이 필요합니다 | [ARM 전용 앱](apps.md#arm-전용-앱)이나 [Android 버전](android-versions.md)을 참고하십시오 |
| `conflicting-signature` | 같은 앱이 다른 키로 서명되어 이미 설치되어 있습니다 | 설치된 앱을 먼저 제거합니다 |
| `downgrade` | 더 새로운 버전이 이미 설치되어 있습니다 | 설치된 앱을 먼저 제거합니다 |
| `invalid-apk` | Android가 읽을 수 있는 서명된 APK가 아닙니다 | APK를 다시 받습니다 |
| `insufficient-storage` | Android에 공간이 부족합니다 | Android의 공간을 확보합니다 |
| `blocked` | 이 Android의 정책이 허용하지 않습니다 | |
| `other` | 그 밖의 경우입니다. Android의 메시지가 뒤에 붙습니다 | |

APK는 2 GiB까지 설치할 수 있습니다. 설치는 한 번에 하나씩 진행되며, 두 번째 설치는 첫 번째가 끝날 때까지
기다립니다.

## 앱 제거

다음 중 하나를 씁니다.

- Windows **설정** > **앱** > **설치된 앱**
- DSA 설정 **앱** 페이지에서 앱 옆의 **제거**
- `dsa apps uninstall <package>`

Android에 기본으로 들어 있는 앱은 제거할 수 없습니다.

---

Android는 Google LLC의 상표입니다. Windows는 Microsoft 그룹의 상표입니다. DSA는 Google, Microsoft와
제휴하거나 그들의 보증 또는 후원을 받지 않습니다.
