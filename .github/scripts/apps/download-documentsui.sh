#!/usr/bin/env bash
# Downloads the image's patched DocumentsUI for one Android version into a folder of default
# apps (make-image.py --default-apps).
#
#   download-documentsui.sh <android major> <out folder>
#
# DocumentsUI builds only in an Android tree, so the Framework build's services job builds it in
# the tree it already has for services.jar and keeps it as
# default-app-documentsui-android<major>-<lock version>. This takes the artifact of that name
# from the newest Framework build run on develop that has one unexpired, whether or not the
# run's other jobs passed: only DocumentsUI's own build decides that the artifact exists. Needs
# GH_TOKEN and GITHUB_REPOSITORY. The APK's record names the patches it was built with, and
# check-image.py refuses an image whose DocumentsUI was built with other patches than the
# repository's, so a run made before dsa/apps/documentsui changed is caught there.
set -euo pipefail

major="$1"
out="$2"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
android="android$major"
version="$(python3 "$repo_root/dsa/guest/framework/framework_lock.py" version "$android")"
name="default-app-documentsui-$android-$version"
echo "looking for $name"
for run in $(gh run list --repo "$GITHUB_REPOSITORY" --workflow framework.yml --branch develop \
               --limit 30 --json databaseId --jq '.[].databaseId'); do
  gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run/artifacts?name=$name" \
    --jq '.artifacts[] | select(.expired | not) | .id' | grep -q . || continue
  echo "run $run has it"
  mkdir -p "$out"
  gh run download "$run" --repo "$GITHUB_REPOSITORY" --name "$name" --dir "$out"
  ls -l "$out"
  exit 0
done
echo "::error::No Framework build run on develop has $name, so the image would have no patched DocumentsUI. Run the Framework build workflow with android set to $android."
exit 1
