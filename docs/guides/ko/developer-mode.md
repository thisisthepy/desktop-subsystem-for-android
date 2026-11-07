# 개발자 모드와 adb

개발자 모드를 켜면 PC의 Android Debug Bridge(adb)가 Desktop Subsystem for Android™(DSA)의 Android에
연결할 수 있습니다. 기본값은 꺼짐입니다.

adb는 DSA에 들어 있지 않습니다. Android SDK Platform-Tools에서 받으십시오.

## adb 연결하기

1. DSA 설정 **고급** 페이지에서 **개발자 모드**를 켭니다. 또는 `dsa settings set developer-mode on`을
   실행합니다.
2. Android가 실행 중이 아니면 시작합니다: `dsa start --wait`
3. 연결합니다.

   ```
   adb connect 127.0.0.1:58526
   adb devices
   ```

   Android에서 연결을 허용하라고 묻지 않습니다. 포트는 이 PC에서만 접속할 수 있으므로 다른 컴퓨터는 연결할
   수 없습니다.
4. 다 쓰면 **개발자 모드**를 다시 끄거나 `dsa settings set developer-mode off`를 실행합니다.

**고급** 페이지의 **adb 연결**에 정확한 명령이 나오고, `dsa settings`도 `Developer mode` 줄에 이 명령을
보여 줍니다.

## 다른 포트 쓰기

기본 포트는 58526입니다. 바꾸려면 **adb 연결**의 **포트**를 고치거나 다음 명령을 실행합니다.

```
dsa settings set adb-port 5555
```

## 개발자 모드와 상관없는 것

DSA는 자기 일에 adb를 쓰지 않습니다. 창, 입력, 설치는 DSA 자체 제어 채널로 오가므로, 개발자 모드를 꺼도
모든 기능이 똑같이 동작합니다.

Android 로그를 보는 데에도 adb가 필요 없습니다. [문제 해결](troubleshooting.md)을 참고하십시오.

---

Android는 Google LLC의 상표입니다. DSA는 Google과 제휴하거나 Google의 보증 또는 후원을 받지 않습니다.
