# README demo — CLI arc, agent-reproducible.
# Regenerate: vhs scripts/readme-demo.tape  (needs a running daemon, logged in,
# and a warmed local cache; uses --source local so it never hits rate limits).
# The TUI hero video is separate and human-recorded: docs/hero-video-script.md.

Output site/public/spotuify-readme-demo.gif

Set FontSize 15
Set Width 1100
Set Height 640
Set Padding 16
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"

Type "spotuify search 'burial' --source local --type track --limit 3"
Sleep 400ms
Enter
Sleep 2s

Type "spotuify play-uri spotify:track:5kadOt1O3LrIV46dns9v7u"
Sleep 400ms
Enter
Sleep 2s

Type "spotuify status --format json | jq '{playing: .is_playing, track: .item.name}'"
Sleep 400ms
Enter
Sleep 3s

Type "spotuify search 'drake' --source local --type track --limit 3 --format ids | spotuify queue add --ids -"
Sleep 400ms
Enter
Sleep 2500ms

Type "spotuify pause"
Sleep 400ms
Enter
Sleep 2s
