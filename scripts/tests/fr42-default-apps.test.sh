#!/usr/bin/env bash
# The default apps (dsa/apps): their pins and patches, and the image workflows that build them
# and hand them to make-image.py, as the guest agent is handed over.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run dsa/apps/test_apps.py

# Every Gradle app apps.json lists is built by both image workflows, and no other.
apps="$(uv run dsa/apps/apps.py list --kind gradle | tr -d '[]" ' )"
for workflow in .github/workflows/test-image.yml .github/workflows/release-image.yml; do
  grep -Fq "app: [${apps//,/, }]" "$workflow" \
    || { echo "$workflow does not build the Gradle apps apps.json lists ($apps)"; exit 1; }
  grep -Fq 'uv run dsa/apps/apps.py fetch ${{ matrix.app }}' "$workflow"
  grep -Fq 'uv run dsa/apps/apps.py build ${{ matrix.app }} --out .scratch/default-apps' "$workflow"
  grep -Fq 'pattern: guest-default-app-*' "$workflow"
  grep -Fq '.github/scripts/apps/download-documentsui.sh ${{ matrix.android }} .scratch/default-apps' "$workflow"
  grep -Fq -- '--default-apps .scratch/default-apps' "$workflow"
done

# DocumentsUI is built in the Framework build's services tree and kept for the image.
grep -Fq '"$DSA_TRANSLATOR_WORK/default-apps"' .github/workflows/framework.yml
grep -Fq 'name: default-app-documentsui-${{ matrix.android }}-${{ steps.pins.outputs.version }}' .github/workflows/framework.yml
grep -Fq 'name="default-app-documentsui-$android-$version"' .github/scripts/apps/download-documentsui.sh

echo "default apps: ok"
