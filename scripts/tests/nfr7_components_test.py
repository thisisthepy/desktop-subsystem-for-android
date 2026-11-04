# /// script
# requires-python = ">=3.11"
# ///
"""Tests for the licence rule on the components Cargo does not list (dsa/third-party.toml).

They load installer/generate-licenses.py and check its rule on made-up components, then
check the real dsa/third-party.toml against the real .csproj and Gradle files.
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "dsa/host/platform/windows/installer/generate-licenses.py"
spec = importlib.util.spec_from_file_location("generate_licenses", SCRIPT)
gl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gl)

POLICY = {"allow": ["MIT", "Apache-2.0", "BSD-3-Clause"], "weak_copyleft": ["MS-RL"],
          "separate_program": ["GPL-2.0-only"]}


def component(**over):
    c = {"name": "thing", "license": "MIT", "part": "settings", "distribution": "linked",
         "source": "https://example.invalid/thing"}
    c.update(over)
    return c


MANIFEST = """\
[policy]
allow = ["MIT"]
weak_copyleft = []
separate_program = []

[[component]]
name = "WPF UI"
package = "WPF-UI"
version = "3.0.5"
license = "MIT"
part = "settings"
distribution = "linked"
source = "https://github.com/lepoco/wpfui"
"""


class ComponentRule(unittest.TestCase):
    def test_nfr7_permissive_licence_is_allowed(self):
        self.assertEqual(gl.component_problems(component(), POLICY), [])

    def test_nfr7_dual_licence_with_one_allowed_side_is_allowed(self):
        self.assertEqual(gl.component_problems(component(license="MIT OR GPL-3.0-only"), POLICY), [])

    def test_nfr7_licence_off_the_allow_list_is_rejected(self):
        problems = gl.component_problems(component(license="GPL-3.0-only"), POLICY)
        self.assertEqual(len(problems), 1)
        self.assertIn("not on the allow list", problems[0])

    def test_nfr7_gpl_linked_into_a_dsa_program_is_rejected(self):
        problems = gl.component_problems(component(license="GPL-2.0-only"), POLICY)
        self.assertIn("separate program", problems[0])

    def test_nfr7_gpl_separate_program_without_source_offer_is_rejected(self):
        problems = gl.component_problems(
            component(license="GPL-2.0-only", distribution="separate-program"), POLICY)
        self.assertIn("source offer", problems[0])

    def test_nfr7_gpl_separate_program_with_source_offer_is_allowed(self):
        c = component(license="GPL-2.0-only", distribution="separate-program", source_offer="offer.txt")
        self.assertEqual(gl.component_problems(c, POLICY), [])

    def test_nfr7_weak_copyleft_needs_an_unmodified_component(self):
        self.assertIn("unmodified", gl.component_problems(component(license="MS-RL"), POLICY)[0])
        self.assertEqual(gl.component_problems(component(license="MS-RL", modified=False), POLICY), [])

    def test_nfr7_component_without_a_source_is_rejected(self):
        problems = gl.component_problems(component(source=""), POLICY)
        self.assertIn("has no source", problems[0])


class ListedPackages(unittest.TestCase):
    def make(self, csproj: str) -> Path:
        root = Path(self.tmp.name)
        (root / "third-party.toml").write_text(MANIFEST, encoding="utf-8")
        (root / "app").mkdir()
        (root / "app/App.csproj").write_text(csproj, encoding="utf-8")
        return root

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.tmp.cleanup()

    def test_nfr7_listed_package_passes(self):
        root = self.make('<PackageReference Include="WPF-UI" Version="3.0.5" />')
        self.assertEqual(gl.check_components(root), [])

    def test_nfr7_unlisted_package_fails(self):
        root = self.make('<PackageReference Include="Newtonsoft.Json" Version="13.0.3" />')
        problems = gl.check_components(root)
        self.assertEqual(len(problems), 1)
        self.assertIn("Newtonsoft.Json 13.0.3 is not listed", problems[0])

    def test_nfr7_package_at_another_version_fails(self):
        root = self.make('<PackageReference Include="WPF-UI" Version="4.0.0" />')
        self.assertEqual(len(gl.check_components(root)), 1)

    def test_nfr7_test_projects_are_left_out(self):
        root = self.make("")
        (root / "app/App.Tests.csproj").write_text(
            '<PackageReference Include="xunit" Version="2.9.2" />', encoding="utf-8")
        self.assertEqual(gl.check_components(root), [])


class RealManifest(unittest.TestCase):
    def test_nfr6_master_notice_carries_the_trademark_notice(self):
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"Android is a trademark of Google LLC."', source)
        self.assertIn("not affiliated with", source.replace('",\n        "', " "))

    def test_nfr7_every_shipped_component_follows_the_licence_rule(self):
        self.assertEqual(gl.check_components(REPO / "dsa"), [])

    def test_nfr7_kernel_source_offer_names_the_pinned_tag(self):
        tag = (REPO / "dsa/kernel/wsl-kernel.tag").read_text(encoding="utf-8").strip()
        offer = gl.kernel_source_offer(REPO / "dsa")
        self.assertIn(tag, offer)
        self.assertIn("three years", offer)

    def test_nfr7_components_notice_lists_every_component(self):
        notice = gl.components_notice(REPO / "dsa")
        for c in gl.load_components(REPO / "dsa")["component"]:
            self.assertIn(c["name"], notice)


if __name__ == "__main__":
    unittest.main()
