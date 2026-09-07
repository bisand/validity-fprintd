# Demo recording for the README.
#
#   sudo pacman -S vhs
#   sudo -k                  # so the first sudo really prompts
#   vhs docs/demo.tape
#
# Touch the sensor whenever the prompt appears. The Sleep values leave room for
# that; lengthen them if you need longer, since VHS records wall-clock time.

Output docs/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1100
Set Height 720
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

# --- The problem: stock fprintd sees nothing -------------------------------
Type "fprintd-list $USER"
Sleep 500ms
Enter
Sleep 3s

# --- The sensor this driver talks to ---------------------------------------
Type "sudo validity-probe"
Sleep 500ms
Enter
# Room for the fingerprint prompt and the probe itself.
Sleep 12s

# --- Enrolled fingers, read off the sensor's own database ------------------
Type "sudo systemctl stop validity-fprintd && sudo validity-db"
Sleep 500ms
Enter
Sleep 12s

Type "sudo systemctl start validity-fprintd"
Sleep 500ms
Enter
Sleep 3s

# --- The point of all this: fingerprint sudo -------------------------------
Type "sudo -k"
Sleep 500ms
Enter
Sleep 1s

Type "sudo ls"
Sleep 500ms
Enter
# Touch the sensor here.
Sleep 12s

Type "journalctl -u validity-fprintd -n 4 --no-pager"
Sleep 500ms
Enter
Sleep 5s
