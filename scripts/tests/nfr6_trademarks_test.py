# /// script
# requires-python = ">=3.11"
# ///
"""Tests for scripts/tests/nfr6_trademarks.py, which checks names and notices against the
trademark rules."""

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
spec = importlib.util.spec_from_file_location("nfr6_trademarks", HERE / "nfr6_trademarks.py")
tm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tm)


class Names(unittest.TestCase):
    def test_nfr6_product_name_is_allowed(self):
        self.assertEqual(tm.name_problems("Desktop Subsystem for Android"), [])
        self.assertEqual(tm.name_problems("DSA Settings"), [])
        self.assertEqual(tm.name_problems("dsa-app-window"), [])

    def test_nfr6_windows_in_a_name_is_rejected(self):
        for name in ("Windows Subsystem for Android", "dsa-windows", "DsaWindowsHost", "WINDOWS"):
            self.assertTrue(tm.name_problems(name), name)

    def test_nfr6_other_microsoft_marks_are_rejected(self):
        for name in ("Microsoft Android Host", "dsa-wsl-bridge", "Hyper-V Android", "Xbox apps"):
            self.assertTrue(tm.name_problems(name), name)

    def test_nfr6_a_mark_inside_a_longer_word_is_not_the_mark(self):
        self.assertEqual(tm.name_problems("Windowsill"), [])

    def test_nfr6_android_first_in_a_name_is_rejected(self):
        self.assertTrue(tm.name_problems("Android Subsystem"))
        self.assertTrue(tm.name_problems("android-host"))

    def test_nfr6_window_title_may_describe_android_but_not_use_a_microsoft_mark(self):
        self.assertEqual(tm.name_problems("Android 앱 설치", title=True), [])
        self.assertTrue(tm.name_problems("Install on Windows", title=True))


class Repository(unittest.TestCase):
    """The checker against a copy of the real surfaces, then one change at a time."""

    FILES = [
        "README.md",
        "docs/locales/README_ko.md",
        f"{tm.SETUP_UI}/Text.cs",
        f"{tm.WINDOWS}/installer/dsa.wxs",
        f"{tm.WINDOWS}/installer/dsa.en-us.wxl",
        f"{tm.WINDOWS}/installer/dsa.ko-kr.wxl",
        f"{tm.SETTINGS}/Strings.resx",
        f"{tm.SETTINGS}/Strings.ko.resx",
        f"{tm.SETTINGS}/Pages/AboutPage.xaml",
        f"{tm.APK_INSTALLER}/InstallerWindow.xaml.cs",
    ]

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        for rel in self.FILES:
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / rel, self.root / rel)

    def tearDown(self):
        self.tmp.cleanup()

    def edit(self, rel, old, new):
        path = self.root / rel
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new), encoding="utf-8")

    def test_nfr6_the_real_surfaces_pass(self):
        self.assertEqual(tm.check(self.root), [])

    def test_nfr6_whole_repository_passes(self):
        self.assertEqual(tm.check(REPO), [])

    def test_nfr6_readme_without_trademark_notice_fails(self):
        self.edit("README.md", "Android is a trademark of Google LLC.", "")
        self.assertTrue(any("README.md" in p for p in tm.check(self.root)))

    def test_nfr6_msi_without_trademark_notice_fails(self):
        self.edit(f"{tm.WINDOWS}/installer/dsa.en-us.wxl", "Android is a trademark of Google LLC.", "")
        self.assertTrue(any("dsa.en-us.wxl" in p for p in tm.check(self.root)))

    def test_nfr6_msi_that_drops_its_localized_comments_fails(self):
        self.edit(f"{tm.WINDOWS}/installer/dsa.wxs", "!(loc.ArpComments)", "DSA")
        self.assertTrue(any("dsa.wxs" in p for p in tm.check(self.root)))

    def test_nfr6_apk_install_window_without_notice_fails(self):
        self.edit(f"{tm.APK_INSTALLER}/InstallerWindow.xaml.cs", '"AboutTrademark.Text"', '"Nothing"')
        self.assertTrue(any("InstallerWindow" in p for p in tm.check(self.root)))

    def test_nfr6_about_page_without_notice_fails(self):
        self.edit(f"{tm.SETTINGS}/Pages/AboutPage.xaml", "AboutIndependent.Text", "AboutSomething.Text")
        self.assertTrue(any("AboutPage" in p for p in tm.check(self.root)))

    def test_nfr6_readme_first_mention_without_trademark_symbol_fails(self):
        self.edit("README.md", "Desktop Subsystem for Android™", "Desktop Subsystem for Android")
        self.assertTrue(any("first mention" in p for p in tm.check(self.root)))

    def test_nfr6_window_title_with_microsoft_mark_fails(self):
        self.edit(f"{tm.SETTINGS}/Strings.resx", "<value>DSA Settings</value>", "<value>DSA for Windows</value>")
        self.assertTrue(any("WindowTitle" in p and "Windows" in p for p in tm.check(self.root)))

    def test_nfr6_service_display_name_with_microsoft_mark_fails(self):
        self.edit(f"{tm.WINDOWS}/installer/dsa.wxs", 'DisplayName="Desktop Subsystem for Android Service"',
                  'DisplayName="Windows Android Service"')
        self.assertTrue(any("ServiceInstall DisplayName" in p for p in tm.check(self.root)))

    def test_nfr6_binary_named_android_first_fails(self):
        crate = self.root / "dsa/host/x"
        (crate / "src/bin").mkdir(parents=True)
        (crate / "Cargo.toml").write_text('[package]\nname = "x"\n', encoding="utf-8")
        (crate / "src/bin/android-host.rs").write_text("fn main() {}\n", encoding="utf-8")
        self.assertTrue(any("android-host" in p for p in tm.check(self.root)))


if __name__ == "__main__":
    unittest.main()
