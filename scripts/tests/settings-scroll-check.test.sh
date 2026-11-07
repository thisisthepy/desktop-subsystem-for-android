#!/usr/bin/env bash
# Checks the short settings window test: the wheel over the page goes over a card, not
# the fixed page title, the wheel is only sent where DSA Settings is under the cursor (and
# a picture of what covers it is kept when it never is), and every navigation item is
# looked for.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-installer.yml"
grep -Fq "PropertyCondition(\$name, 'State')" "$workflow"
grep -Fq 'WindowFromPoint(POINT point)' "$workflow"
grep -Fq 'if ($cover.ProcessId -eq $owner)' "$workflow"
grep -Fq 'screens\settings-covered.png' "$workflow"
grep -Fq 'another window covers DSA Settings where the wheel goes' "$workflow"
for item in "'Android'" "'Network'"; do
  grep -F '$items = ' "$workflow" | grep -Fq "$item"
done
echo "settings scroll check: ok"
# Keys go to the foreground window: the keyboard check brings DSA Settings to the front
# first, since the ARM64 runner's Start menu takes the keyboard.
grep -Fq 'function Set-Foreground($Window)' "$workflow"
test "$(grep -c 'Set-Foreground $window' "$workflow")" -ge 2
# Tab goes on until every item was in view, since About in the footer can come after the page.
grep -Fq 'for ($i = 0; $i -lt 40; $i++)' "$workflow"
