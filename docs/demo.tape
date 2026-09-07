# Demo recording for the README.
#
#   sudo pacman -S vhs
#   sudo -v                  # cache credentials; nothing here should prompt
#   vhs docs/demo.tape
#
# One touch, ~22 seconds in, when validity-verify prints
#
#     >>> Touch the fingerprint sensor now (30s timeout) <<<
#
# Use a finger that is actually enrolled (`fprintd-list $USER` will tell you).
#
# This deliberately avoids demonstrating PAM: a live sudo exchange retries and
# falls back to a password prompt, which makes the recording unpredictable.
# validity-verify has one clear prompt and a generous window.

Output docs/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1100
Set Height 700
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

# --- The problem: stock fprintd sees nothing -------------------------------
Type "fprintd-list $USER"
Sleep 500ms
Enter
Sleep 3s

# --- The sensor this driver talks to ---------------------------------------
Type "clear && sudo validity-probe | head -20"
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
