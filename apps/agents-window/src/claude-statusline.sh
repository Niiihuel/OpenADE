#!/bin/sh
# openADE: keeps the plan limits Claude Code reports to its status line, for
# the usage panel. Reads stdin, writes one file beside itself, prints one line.
# No network; never fails.
dir=${0%/*}
state="$dir/claude-statusline.json"
input=$(tr -d '\000' 2>/dev/null) || input=

case "$input" in
*'"rate_limits"'*)
  # At most one write every 15 s: Claude Code runs this several times a second.
  now=$(date +%s 2>/dev/null)
  last=$(stat -c %Y "$state" 2>/dev/null || stat -f %m "$state" 2>/dev/null)
  if [ -z "$now" ] || [ -z "$last" ] || [ $((now - last)) -ge 15 ] 2>/dev/null; then
    umask 077
    printf '%s' "$input" >"$state.$$" 2>/dev/null && mv -f "$state.$$" "$state" 2>/dev/null
    rm -f "$state.$$" 2>/dev/null
  fi
  ;;
esac

# shortcut: sed over one-line JSON, which is what Claude Code sends; a
# pretty-printed payload only loses the percentage. Use jq if that changes.
field() {
  printf '%s' "$input" | sed -n "s/.*\"$1\"[^}]*\"used_percentage\"[[:space:]]*:[[:space:]]*\([0-9]\{1,3\}\).*/\1/p" | head -n 1
}
line=$(printf '%s' "$input" | sed -n 's/.*"display_name"[[:space:]]*:[[:space:]]*"\([^"\\]*\)".*/\1/p' | head -n 1)
five=$(field five_hour)
week=$(field seven_day)
limit=
[ -n "$five" ] && limit="5 h $five%"
if [ -n "$week" ] && { [ -z "$five" ] || [ "$week" -gt "$five" ]; } 2>/dev/null; then
  limit="7 d $week%"
fi
[ -n "$line" ] && [ -n "$limit" ] && line="$line · $limit"
printf '%s\n' "${line:-${limit:-openADE}}"
exit 0
