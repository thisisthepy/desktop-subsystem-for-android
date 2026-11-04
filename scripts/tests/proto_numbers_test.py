"""Tests for scripts/tests/proto_numbers.py, which finds number and name clashes in .proto files."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("proto_numbers", HERE / "proto_numbers.py")
proto_numbers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proto_numbers)

CLEAN = """\
syntax = "proto3";
package dsa.control;

// A comment that says field = 1; is not a field.
message Settings {
  bool clipboard = 1;
  repeated string links = 2; /* = 2 in a block comment */
  reserved 3;
  message Inner { int32 a = 1; }
  Inner inner = 4;
}

enum Mode {
  option allow_alias = true;
  MODE_UNSPECIFIED = 0;
  MODE_DEFAULT = 0;
}

message Envelope {
  uint32 sequence = 1;
  oneof body {
    Settings settings = 140;
    Mode mode = 141;
  }
}

message Other { message Inner { int32 a = 1; } }
"""


def problems(text: str) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "control.proto"
        path.write_text(text, encoding="utf-8")
        return proto_numbers.check(path)


class ProtoNumbers(unittest.TestCase):
    def test_a_clean_definition_has_no_clash(self):
        self.assertEqual(problems(CLEAN), [])

    def test_two_fields_of_a_message_with_one_number_clash(self):
        found = problems(CLEAN.replace("Inner inner = 4;", "Inner inner = 4;\n  bool dark = 1;"))
        self.assertEqual(len(found), 1)
        self.assertIn("message Settings gives", found[0])
        self.assertIn("number 1 to both clipboard and dark", found[0])

    def test_two_envelope_entries_with_one_number_clash(self):
        found = problems(CLEAN.replace("Mode mode = 141;", "Mode mode = 140;"))
        self.assertEqual(len(found), 1)
        self.assertIn("Envelope", found[0])
        self.assertIn("number 140 to both settings and mode", found[0])

    def test_a_oneof_entry_and_a_plain_field_with_one_number_clash(self):
        found = problems(CLEAN.replace("Mode mode = 141;", "Mode mode = 1;"))
        self.assertEqual(len(found), 1)
        self.assertIn("number 1 to both sequence and mode", found[0])

    def test_a_message_defined_twice_clashes(self):
        found = problems(CLEAN + "\nmessage Settings { bool other = 1; }\n")
        self.assertEqual(found, [f"{found[0].split(':')[0]}: message Settings is defined twice"])

    def test_messages_of_one_name_inside_different_messages_do_not_clash(self):
        self.assertEqual(problems(CLEAN), [])

    def test_enum_values_with_one_number_clash_without_allow_alias(self):
        found = problems(CLEAN.replace("  option allow_alias = true;\n", ""))
        self.assertEqual(len(found), 1)
        self.assertIn("enum Mode gives value number 0 to both", found[0])

    def test_the_checked_in_definitions_have_no_clash(self):
        protocol = HERE.parent.parent / "dsa" / "protocol"
        for path in sorted(protocol.glob("*.proto")):
            self.assertEqual(proto_numbers.check(path), [], path.name)


if __name__ == "__main__":
    unittest.main()
