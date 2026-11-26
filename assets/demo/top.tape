# Records assets/demo/top.gif from simulated groups: no Docker, no daemon.
# Run from the repository root (SDS_VERSION is what the header shows):
#   SDS_VERSION=x.y.z cargo build --release
#   PATH="$PWD/target/release:$PATH" vhs assets/demo/top.tape
Output assets/demo/top.gif

Require sds

Set Shell bash
Set FontSize 14
Set Width 1280
Set Height 620
Set Padding 16
Set WindowBar Colorful
Set BorderRadius 8
Set Margin 0
Set Theme "Catppuccin Mocha"
Set Framerate 12
Set TypingSpeed 60ms
Set CursorBlink false

Hide
Type "export PS1='$ ' && clear"
Enter
Sleep 500ms
Show

Type "sds top --demo"
Sleep 600ms
Enter

# "blog" has been idle for 30 s and goes to sleep on its own
Sleep 5s

# wake "ml-lab" by hand
Down 4
Sleep 1s
Type "w"
Sleep 8500ms

# back to "blog": the next request wakes it, the sparkline fills
Up@250ms 4
Sleep 10s
Screenshot assets/demo/top.png
Sleep 5s

# put it back to sleep
Type "s"
Sleep 4s
Type "q"
Sleep 800ms
