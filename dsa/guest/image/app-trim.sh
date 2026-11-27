# Desktop Subsystem for Android (DSA)
# Trims and compacts an app no window of which the user can see, as the agent asks
# (vendor.dsa.app.trim, dsa-app-memory.rc): "<number> <HIDDEN|BACKGROUND> <compact> <process>".
# The activity manager sends the process the trim level (ComponentCallbacks2.onTrimMemory), on
# which ART and the UI toolkit let caches go; it sends a level only once, and never a lower one
# than the process last had, so asking again costs nothing. With compact 1 the activity manager
# then compacts the process fully, which pages its anonymous memory and its files out at once.
#
# Android still shows the task of an app the host hides, so its process stays in the foreground,
# and "am send-trim-memory" refuses it a background level. DSA's framework adds the activity
# manager's "dsa-hidden-trim", which sends the level to a process whose package the agent said
# the host hides; on a framework without it, which answers "Unknown command", the trim goes
# through send-trim-memory as before. Whatever the activity manager answers is logged as it
# said it, without the stack frames of an exception.
#
# Once the request is carried out, vendor.dsa.app.trim.done is "<number> <result>", so the agent
# sends the next one:
#   sent               the activity manager sent the trim
#   refused <reason>   it did not; the reason is the first line of its answer, cut short to fit
#                      a property
#   invalid            the request names no level or process the script takes

tag=DsaAppTrim

# What a command answered, for the log, on one line: its lines without the stack frames of an
# exception, which say nothing the exception's own line does not, and without empty lines.
shown() {
    printf '%s\n' "$1" | grep -v -e '^[[:space:]]*at ' -e '^[[:space:]]*$' | tr '\n' ' ' | sed 's/ *$//'
}

# Why a command failed, in a few words: the message of its error or exception, or else its
# first line, at most 64 characters so the answer fits in a property.
reason() {
    why=$(printf '%s\n' "$1" | sed -n -e 's/^Error: //p' -e 's/^[A-Za-z0-9_.$]*Exception: //p' | head -n 1)
    [ -n "$why" ] || why=$(printf '%s\n' "$1" | grep -v '^[[:space:]]*$' | head -n 1)
    [ -n "$why" ] || why="exit status $2"
    printf '%s' "$why" | tr -c 'A-Za-z0-9 ._:,()/-' ' ' | cut -c1-64
}

# Whether a command's answer says it failed: some exit 0 all the same.
failed() {
    [ "$2" != 0 ] && return 0
    case "$1" in
        *Error* | *Exception* | *Unknown* | *Unable*) return 0 ;;
    esac
    return 1
}

trim() {
    set -- $1
    number=$1
    level=$2
    compact=$3
    process=$4
    result=invalid
    case "$level" in
        HIDDEN | BACKGROUND) ;;
        *) log -p w -t "$tag" "ignored a request with the level '$level'"; return ;;
    esac
    case "$process" in
        "" | *[!A-Za-z0-9._:]*) log -p w -t "$tag" "ignored a request for the process '$process'"; return ;;
    esac
    if [ "$(cmd activity dsa-hidden-trim 2>/dev/null)" = supported ]; then
        how="dsa-hidden-trim"
        out=$(cmd activity dsa-hidden-trim send "$process" "$level" 2>&1)
        status=$?
    else
        how="send-trim-memory"
        out=$(am send-trim-memory "$process" "$level" 2>&1)
        status=$?
    fi
    if failed "$out" "$status"; then
        result="refused $(reason "$out" "$status")"
        log -p w -t "$tag" "the activity manager did not send $process the memory trim $level ($how, exit status $status): $(shown "$out")"
    else
        result=sent
        log -t "$tag" "sent $process the memory trim $level ($how)${out:+: $(shown "$out")}"
    fi
    if [ "$compact" = 1 ]; then
        out=$(am compact full "$process" 2>&1)
        status=$?
        if failed "$out" "$status"; then
            log -p w -t "$tag" "could not compact $process (exit status $status): $(shown "$out")"
        else
            log -t "$tag" "compacted $process${out:+: $(shown "$out")}"
        fi
    fi
}

while :; do
    asked=$(getprop vendor.dsa.app.trim)
    trim "$asked"
    setprop vendor.dsa.app.trim.done "${asked%% *} $result"
    [ "$(getprop vendor.dsa.app.trim)" = "$asked" ] && break
done
