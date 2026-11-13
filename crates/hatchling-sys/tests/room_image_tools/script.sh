# What a shell script does all day: functions, pipes, a here-document, a
# job in the background and a wait for it, a trap, a timed read.
set -e
f() { echo "$1"; }
x=$(f a | tr a b)
cat <<EOT > heredoc.txt
$x
EOT
(sleep 0.1; echo sub) & wait
trap "echo bye" EXIT
read -r -t 1 line < letters.txt
ulimit -n > /dev/null
for i in 1 2 3; do :; done
[[ -f heredoc.txt ]] && echo ok | while read -r l; do echo "$l"; done
exec 3< letters.txt; exec 3<&-
time true
