# Reproducible TUI demo for ripsync, rendered by charmbracelet VHS.
#   vhs docs/demo.tape   ->   docs/assets/ripsync-demo.gif
#
# Requires: vhs, plus a release ripsync on PATH (cargo build --release).

Output docs/assets/ripsync-demo.gif

Set Shell "bash"
Set FontSize 18
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set Padding 20

Hide
Type "export PATH=$PWD/target/release:$PATH" Enter
Type "rm -rf /tmp/ripsync-demo && mkdir -p /tmp/ripsync-demo/src" Enter
Type "for i in $(seq 1 2000); do printf 'data %d' $i > /tmp/ripsync-demo/src/file$i.txt; done" Enter
Type "clear" Enter
Show

Type "ripsync /tmp/ripsync-demo/src /tmp/ripsync-demo/dst --verify changed"
Sleep 700ms
Enter

# Let the planning/copy/verify phases play out in the TUI.
Sleep 4s

# Cycle through the views, then quit.
Type "2"
Sleep 1s
Type "3"
Sleep 1s
Type "q"
Sleep 700ms

Sleep 1s
