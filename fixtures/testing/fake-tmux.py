#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import sys
import time

args = sys.argv[1:]
if args == ["-V"]:
    print("tmux 3.7b")
    raise SystemExit(0)

socket = args[args.index("-S") + 1]
args = args[args.index("-f") + 2:]
state_path = socket + ".fake.json"

def load():
    try:
        with open(state_path) as source:
            return json.load(source)
    except FileNotFoundError:
        return {}

def save(state):
    os.makedirs(os.path.dirname(state_path), exist_ok=True)
    # Concurrent invocations (poll loops racing an attach client) must not
    # share one temp file, or the loser's os.replace crashes.
    temporary = "{}.tmp.{}".format(state_path, os.getpid())
    with open(temporary, "w") as destination:
        json.dump(state, destination)
    os.replace(temporary, state_path)

def save_if_changed(state, before):
    if json.dumps(state, sort_keys=True) != before:
        save(state)

def snapshot(state):
    return json.dumps(state, sort_keys=True)

def each_session(state):
    for name, entry in list(state.items()):
        if name.startswith("_") or not isinstance(entry, dict) or "pid" not in entry:
            continue
        yield name, entry

def missing_session(name):
    print("can't find session: " + name, file=sys.stderr)
    raise SystemExit(1)

def process_is_gone(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    # Linux zombies still accept kill(pid, 0). Treat them as dead so stop()
    # can observe pane exit when the reaper is slow (this test environment).
    if not os.path.isdir("/proc"):
        return False
    try:
        with open(f"/proc/{pid}/stat") as source:
            text = source.read()
    except FileNotFoundError:
        return True
    rparen = text.rfind(")")
    return rparen != -1 and text[rparen + 2 : rparen + 3] == "Z"

def refresh(entry):
    if entry.get("survive_stop"):
        return entry
    exit_path = entry["exit_path"]
    if os.path.exists(exit_path):
        with open(exit_path) as source:
            entry["status"] = int(source.read() or "0")
        entry["dead"] = True
        entry["dead_at"] = entry["dead_at"] or int(time.time())
    elif not entry["dead"] and process_is_gone(entry["pid"]):
        entry["status"] = 137
        entry["dead"] = True
        entry["dead_at"] = int(time.time())
        entry["signal"] = 9
    return entry

def client_is_utf8():
    # Real tmux sanitizes non-printable output (including the \x1f field
    # separator) to '_' unless the client environment advertises UTF-8.
    for key in ("LC_ALL", "LC_CTYPE", "LANG"):
        value = os.environ.get(key, "")
        if value:
            lower = value.lower()
            return "utf-8" in lower or "utf8" in lower
    return False

def session_row(name, entry):
    status = "" if entry["status"] is None else str(entry["status"])
    row = "\x1f".join([
        name,
        "1" if entry["dead"] else "0",
        status,
        str(entry["cols"]),
        str(entry["rows"]),
        str(entry["activity"]),
        str(entry["attached"]),
        str(entry["pid"]),
        "" if entry["dead_at"] is None else str(entry["dead_at"]),
        "" if entry["signal"] is None else str(entry["signal"])
    ])
    if not client_is_utf8():
        row = row.replace("\x1f", "_")
    return row

state = load()
command = args.pop(0)

if command == "new-session":
    session = None
    cols = 80
    rows = 24
    cwd = os.getcwd()
    environment = os.environ.copy()
    index = 0
    while index < len(args):
        value = args[index]
        if value == "--":
            child = args[index + 1:]
            break
        if value == "-d":
            index += 1
            continue
        if value in ("-s", "-x", "-y", "-c", "-e"):
            argument = args[index + 1]
            if value == "-s":
                session = argument
            elif value == "-x":
                cols = int(argument)
            elif value == "-y":
                rows = int(argument)
            elif value == "-c":
                cwd = argument
            else:
                key, setting = argument.split("=", 1)
                environment[key] = setting
            index += 2
            continue
        index += 1
    exit_path = socket + "." + session + ".exit"
    wrapper = [
        "/bin/sh", "-c",
        '"$@"; code=$?; printf "%s" "$code" > "$0"; exit "$code"',
        exit_path, *child
    ]
    process = subprocess.Popen(
        wrapper,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True
    )
    state[session] = {
        "pid": process.pid,
        "cols": cols,
        "rows": rows,
        "activity": int(time.time()),
        "attached": 0,
        "dead": False,
        "status": None,
        "dead_at": None,
        "signal": None,
        "exit_path": exit_path,
        "screen": "",
        "sent_keys": [],
        "pasted": [],
        "buffers": {}
    }
    save(state)
elif command == "list-sessions":
    # Read paths persist only real refresh transitions; unconditional saves
    # from concurrent pollers would revert another process's write.
    before = snapshot(state)
    for name, entry in each_session(state):
        refresh(entry)
        print(session_row(name, entry))
    save_if_changed(state, before)
elif command == "display-message":
    session = args[args.index("-t") + 1]
    if session not in state or not isinstance(state.get(session), dict) or "pid" not in state[session]:
        missing_session(session)
    before = snapshot(state)
    entry = refresh(state[session])
    print(session_row(session, entry))
    save_if_changed(state, before)
elif command == "capture-pane":
    session = args[args.index("-t") + 1]
    print(state[session].get("screen", ""), end="")
elif command == "send-keys":
    session = args[args.index("-t") + 1]
    if state[session].get("fail_send_keys"):
        print("forced send-keys failure", file=sys.stderr)
        raise SystemExit(1)
    keys = args[args.index("--") + 1:]
    state[session].setdefault("sent_keys", []).extend(keys)
    save(state)
elif command == "load-buffer":
    name = args[args.index("-b") + 1]
    buffers = state.setdefault("_buffers", {})
    buffers[name] = sys.stdin.read()
    save(state)
elif command == "paste-buffer":
    session = args[args.index("-t") + 1]
    name = args[args.index("-b") + 1]
    buffers = state.setdefault("_buffers", {})
    state[session].setdefault("pasted", []).append(buffers[name])
    if "-d" in args:
        del buffers[name]
    save(state)
elif command == "delete-buffer":
    name = args[args.index("-b") + 1]
    state.setdefault("_buffers", {}).pop(name, None)
    save(state)
elif command == "has-session":
    session = args[args.index("-t") + 1]
    if session in state and isinstance(state[session], dict) and "pid" in state[session]:
        raise SystemExit(0)
    missing_session(session)
elif command == "attach-session":
    session = args[args.index("-t") + 1]
    if session not in state or not isinstance(state.get(session), dict) or "pid" not in state[session]:
        missing_session(session)
    entry = state[session]
    if entry.get("fail_attach_remove"):
        del state[session]
        save(state)
        missing_session(session)
    if entry.get("fail_attach"):
        print("unable to attach to session " + session, file=sys.stderr)
        raise SystemExit(1)
    state["_last_attach"] = session
    entry["attached"] = entry.get("attached", 0) + 1
    save(state)
    # Echo mode makes the fake behave like a client that is actually reading
    # the terminal: the gateway's relay is only proven if bytes travel from the
    # WebSocket, through the PTY, into this process, and back out.
    if os.environ.get("LATCH_FAKE_TMUX_ECHO"):
        os.write(1, b"<attached>")
        while True:
            try:
                chunk = os.read(0, 1024)
            except OSError:
                break
            if not chunk:
                break
            os.write(1, b"<echo>" + chunk)
    raise SystemExit(0)
elif command == "resize-window":
    session = args[args.index("-t") + 1]
    state[session]["cols"] = int(args[args.index("-x") + 1])
    state[session]["rows"] = int(args[args.index("-y") + 1])
    state[session]["activity"] = int(time.time())
    save(state)
elif command == "set-option":
    pass
elif command == "kill-session":
    session = args[args.index("-t") + 1]
    entry = state.pop(session, None)
    if entry and not refresh(entry)["dead"]:
        try:
            os.killpg(entry["pid"], signal.SIGKILL)
        except ProcessLookupError:
            pass
    save(state)
else:
    print("unsupported fake tmux command: " + command, file=sys.stderr)
    raise SystemExit(2)
