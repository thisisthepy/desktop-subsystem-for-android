"""Finds number and name clashes in the protocol definitions.

Pull requests made side by side tend to pick the same next field number, or add a message
under a name another one also adds. Git merges such changes without a conflict when they
land on different lines, and the generated code then mixes two messages up at run time.
This fails when, in any .proto file given:

- a message has two fields with the same number, counting the fields of its oneofs (so two
  Envelope entries cannot share a number);
- an enum has two values with the same number (unless it sets allow_alias);
- a message, or an enum, or two of them, share a full name.

Usage: uv run scripts/tests/proto_numbers.py FILE.proto...
"""

import re
import sys
from pathlib import Path

FIELD = re.compile(r"^(?:repeated\s+|optional\s+)?[A-Za-z_][\w.]*\s+([A-Za-z_]\w*)\s*=\s*(\d+)")
MAP_FIELD = re.compile(r"^map\s*<[^>]*>\s+([A-Za-z_]\w*)\s*=\s*(\d+)")
ENUM_VALUE = re.compile(r"^([A-Za-z_]\w*)\s*=\s*(-?\d+)")
OPEN = re.compile(r"^(message|enum|oneof)\s+([A-Za-z_]\w*)\s*\{")


def strip_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


def statements(text: str):
    """Splits the file into `name {`, `}` and `...;` statements, in order."""
    for match in re.finditer(r"[^{};]*[{};]", text):
        statement = " ".join(match.group(0).split())
        if statement not in ("", ";"):
            yield statement


def check(path: Path) -> list[str]:
    problems = []
    # Each open scope: (kind, full name, {number: field}, allow_alias).
    stack: list[list] = []
    names: dict[str, str] = {}
    for statement in statements(strip_comments(path.read_text(encoding="utf-8"))):
        opened = OPEN.match(statement)
        if opened:
            kind, name = opened.groups()
            if kind == "oneof":
                # A oneof's fields are numbered in the message around it.
                stack.append(["oneof", stack[-1][1] if stack else name, None, False])
                continue
            outer = next((s[1] for s in reversed(stack) if s[0] != "oneof"), "")
            full = f"{outer}.{name}" if outer else name
            if full in names:
                problems.append(f"{path}: {kind} {full} is defined twice")
            names[full] = kind
            stack.append([kind, full, {}, False])
            continue
        if statement == "}":
            if stack:
                stack.pop()
            continue
        if statement.endswith("{"):
            # Any other block (a service, an extend) is followed for its closing brace only.
            stack.append(["other", stack[-1][1] if stack else "", None, False])
            continue
        if not stack:
            continue
        body = statement.rstrip(";").strip()
        if body.startswith(("reserved", "option", "extensions")):
            if body.replace(" ", "") == "optionallow_alias=true" and stack[-1][0] == "enum":
                stack[-1][3] = True
            continue
        scope = next((s for s in reversed(stack) if s[0] != "oneof"), None)
        if scope is None or scope[0] == "other":
            continue
        if scope[0] == "enum":
            value = ENUM_VALUE.match(body)
            if not value:
                continue
            name, number = value.group(1), int(value.group(2))
        else:
            field = FIELD.match(body) or MAP_FIELD.match(body)
            if not field:
                continue
            name, number = field.group(1), int(field.group(2))
        numbers = scope[2]
        if number in numbers and not (scope[0] == "enum" and scope[3]):
            what = "value" if scope[0] == "enum" else "field"
            problems.append(
                f"{path}: {scope[0]} {scope[1]} gives {what} number {number} to both "
                f"{numbers[number]} and {name}"
            )
        else:
            numbers[number] = name
    return problems


def main(paths: list[str]) -> int:
    problems = [problem for path in paths for problem in check(Path(path))]
    for problem in problems:
        print(problem)
    if problems:
        print(f"{len(problems)} clash(es); pick numbers and names nothing else uses")
        return 1
    print(f"no clashes in {len(paths)} file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
