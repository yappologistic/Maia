#!/bin/sh
# The issue labels, made or brought up to date on GitHub with gh. GitHub's own labels
# that have a place here are renamed rather than replaced, so issues keep them.
#
#   tools/labels.sh
set -e
R=yappologistic/Maia

rename() {
  if gh label list -R "$R" --limit 200 --json name --jq '.[].name' | grep -qx "$1"; then
    gh label edit "$1" -R "$R" --name "$2" >/dev/null && echo "renamed $1 -> $2"
  fi
}
rename bug "type: bug"
rename enhancement "type: idea"
rename duplicate "status: duplicate"
rename wontfix "status: won't fix"

while IFS='|' read -r name color desc; do
  [ -n "$name" ] || continue
  gh label create "$name" -R "$R" --color "$color" --description "$desc" --force >/dev/null && echo "$name"
done <<'LABELS'
type: bug|d73a4a|Something is broken.
type: crash|b60205|Maia closes or freezes.
type: looks wrong|e99695|Drawn wrong: overlaps, cut-off text, wrong colours.
type: slow|fbca04|Slow, laggy, or using too much memory or CPU.
type: idea|a2eeef|Something Maia could do, or do better.
found by: person|0075ca|Found and reported by a person.
found by: AI|8250df|Found by an AI; no person checked the report.
found by: AI + person|5a32a3|Found by an AI and checked by a person before filing.
model: Claude|d97757|The AI was a Claude model.
model: GPT / Codex|10a37f|The AI was a GPT or Codex model.
model: Gemini|4285f4|The AI was a Gemini model.
model: Grok|444444|The AI was a Grok model.
model: open model|6e7781|The AI was an open model: Llama, Qwen, DeepSeek and the like.
model: other|bfbfbf|Another AI; the report names it.
proof: steps|c2e0c6|Comes with steps that make it happen again.
proof: screenshots / video|bfdadc|Comes with screenshots or a video.
proof: test|0e8a16|Comes with a test that fails.
proof: none|eeeeee|Comes with no proof yet.
status: needs triage|fbca04|Not looked at yet.
status: needs info|d876e3|Waiting for more from the reporter.
status: confirmed|0e8a16|Reproduced by the maintainer.
status: can't reproduce|cfd3d7|Didn't happen when the maintainer tried it.
status: fixed in next release|1d76db|Fixed; ships in the next release.
status: won't fix|ffffff|Won't be worked on.
status: duplicate|cccccc|Already reported.
claim: held up|2da44e|Checked: what the report said was true.
claim: partly|bf8700|Checked: some of what the report said was true.
claim: wrong|cf222e|Checked: what the report said was not true.
area: files & copies|c5def5|Listings, copying, moving, renaming, the trash.
area: preview & viewer|c5def5|Previews, the picture viewer, thumbnails.
area: drives & mounts|c5def5|Disks, USB drives, mounting, encrypted volumes.
area: archives|c5def5|Zip, tar, 7z and the like.
area: network|c5def5|Network places and shares.
area: sidebar & views|c5def5|The sidebar, views, tabs and panels.
area: search|c5def5|Filter, search and Go To.
area: themes & look|c5def5|Themes, fonts, animations.
area: GitHub page|c5def5|The GitHub profile page.
area: install & updates|c5def5|Packages, installing, updating.
on: Wayland|ededed|Seen on Wayland.
on: X11|ededed|Seen on X11.
LABELS
