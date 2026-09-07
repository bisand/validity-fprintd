# Demo recording for the README.
#
#   sudo pacman -S vhs
#   sudo -v                  # cache credentials; nothing here should prompt
#   vhs docs/demo.tape
#
# One touch, ~18 seconds in, when validity-verify prints
#
#     >>> Touch the fingerprint sensor now (30s timeout) <<<
#
# Use a finger that is actually enrolled; `fprintd-list $USER` will tell you.
#
# Deliberately avoids demonstrating PAM: a live sudo exchange retries and falls
# back to a password prompt, which makes the recording unpredictable.
# validity-verify has one clear prompt and a generous window.

Output docs/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1100
Set Height 760
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

# --- What the sensor is, and whether it is paired to this machine ----------
Type "sudo validity-probe"
Sleep 500ms
Enter
Sleep 9s

# --- Capture and match, on the chip ----------------------------------------
# The daemon holds the USB interface, so it steps aside for the CLI tool.
Type "clear && sudo systemctl stop validity-fprintd && sudo validity-verify"
Sleep 500ms
Enter
# Touch the sensor when the prompt appears.
Sleep 22s

Type "sudo systemctl start validity-fprintd"
Sleep 500ms
Enter
Sleep 4s
