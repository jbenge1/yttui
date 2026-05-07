# vhs tape for the yttui demo. Regenerate with:
#   vhs docs/demo.tape
#
# Outputs docs/demo.gif. The Set lines below only style the recording —
# yttui itself runs the same regardless of theme/font/size.
#
# Needs network: the search is real (yt-dlp hits YouTube).

Output docs/demo.gif

Set FontSize 14
Set Width 1100
Set Height 600
Set Padding 16
Set Theme "TokyoNight"
Set TypingSpeed 60ms

# Query → results
Type "yttui factorio balancer"
Sleep 400ms
Enter
Sleep 4s

# j/k navigation
Type "jjj"
Sleep 600ms
Type "k"
Sleep 600ms

# Filter mode
Type "/"
Sleep 300ms
Type "balancer"
Sleep 1500ms
Escape
Sleep 600ms

# Help overlay
Type "?"
Sleep 1500ms
Escape
Sleep 400ms

# Quit
Type "q"
Sleep 600ms
