#!/usr/bin/env python3
"""Disposable, real HTTP Git verification. No production repositories/remotes.

Run from the repository root after `cargo build -p latch`:
  python3 docs/verification/git-management-2026-10-13/live_gateway.py
Add --keep for the temporary simulator harness (Ctrl-C stops its gateway).
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[3]
OUT = Path(__file__).resolve().parent
base = Path(tempfile.mkdtemp(prefix="latch-git-verification-", dir="/tmp")).resolve()
home, repo, bare, other = [base / name for name in ("home", "work", "remote.git", "other")]
home.mkdir()
repo.mkdir()
env = dict(os.environ, LATCH_HOME=str(home), GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
events = []


def git(cwd, *args):
    return subprocess.check_output(["git", "-C", str(cwd), *args], env=env, stderr=subprocess.PIPE).decode().strip()


def configure(cwd):
    for key, value in [("user.name", "Latch Verification"), ("user.email", "verification@example.invalid"),
                       ("commit.gpgsign", "false"), ("push.gpgSign", "false"),
                       ("core.hooksPath", str(cwd / ".git" / "hooks")), ("credential.helper", "")]:
        git(cwd, "config", key, value)


def commit(cwd, name, text, message):
    (cwd / name).write_text(text)
    git(cwd, "add", "--", name)
    git(cwd, "commit", "-qm", message)


git(repo, "init", "-qb", "main")
configure(repo)
commit(repo, "README.md", "Initial\n", "Initial")
git(base, "init", "-q", "--bare", "-b", "main", str(bare))
git(repo, "remote", "add", "origin", str(bare))
git(repo, "push", "-qu", "origin", "main")
git(base, "clone", "-q", str(bare), str(other))
configure(other)
token = uuid.uuid4().hex + uuid.uuid4().hex
token_file = home / "serve.token"
token_file.write_text(token)
token_file.chmod(0o600)
ready = base / "ready.json"
gateway_log = open(base / "gateway.log", "w")
server = subprocess.Popen([str(ROOT / "target/debug/latch"), "serve", "--bind", "127.0.0.1:0",
                           "--ready-file", str(ready)], env=env, stdout=gateway_log, stderr=gateway_log)


def api(route, body=None, device="phone-a", grant="control", **query):
    target = route + (("?" + urllib.parse.urlencode(query)) if query else "")
    headers = {"Authorization": "Bearer " + token, "x-latch-device-id": device,
               "x-latch-device-grant": grant, "Content-Type": "application/json"}
    request = urllib.request.Request(url + target, data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        response = urllib.request.urlopen(request, timeout=35)
    except urllib.error.HTTPError as error:
        response = error
    raw = response.read()
    assert len(raw) < 512 * 1024, (target, len(raw))
    result = json.loads(raw)
    events.append({"route": route, "method": request.get_method(), "device": device, "grant": grant,
                   "http": response.code, "bytes": len(raw), "code": result.get("code", result.get("error")),
                   "state": result.get("state"), "outcome": (result.get("outcome") or {}).get("code")})
    return response.code, result


def status(path=repo):
    code, result = api("/v2/git/status", path=str(path))
    assert code == 200, result
    return result


def mutate(route_name, path=repo, expected=None, **args):
    request = dict(requestId=str(uuid.uuid4()), path=str(path), expected=expected or status(path)["preconditions"], **args)
    code, receipt = api("/v2/git/" + route_name, request)
    assert code in (200, 202), receipt
    while receipt["state"] == "pending":
        time.sleep(0.1)
        code, receipt = api("/v2/git/receipts/" + request["requestId"])
        assert code == 200, receipt
    return request, receipt


def completed(receipt, code=None):
    assert receipt["state"] == "completed", receipt
    if code:
        assert receipt["outcome"]["code"] == code, receipt


try:
    for _ in range(100):
        if ready.exists():
            break
        assert server.poll() is None, "gateway stopped"
        time.sleep(0.1)
    url = json.loads(ready.read_text())["url"]
    assert api("/v2/capabilities")[0] == 200
    assert api("/v2/git/status", path=str(repo), grant="observe")[0] == 403
    assert api("/v2/git/status", path=str(repo), grant="interact")[0] == 403
    status()
    # Opening from one device makes the Mac's recent list visible to another.
    code, listing = api("/v2/git/repositories", device="phone-b")
    assert code == 200 and any(item["root"] == str(repo) for item in listing["recent"])
    fetched = status()["lastFetchedAt"]
    (repo / "odd \"name\"\n雪.txt").write_text("untracked\n")
    (repo / "README.md").write_text("Modified\n")
    current = status()
    assert current["lastFetchedAt"] == fetched
    for entry in current["entries"]:
        assert api("/v2/git/diff", path=str(repo), file=entry["pathKey"], area=entry["area"])[0] == 200
    request, receipt = mutate("stage", all=["unstaged", "untracked"])
    completed(receipt, "staged")
    assert api("/v2/git/stage", request)[1] == receipt, "same ID replays the receipt"
    assert api("/v2/git/receipts/" + request["requestId"], device="phone-b")[0] == 404
    assert api("/v2/git/receipts/" + request["requestId"], grant="observe")[0] == 403
    changed = dict(request, all=["untracked"])
    assert api("/v2/git/stage", changed)[0] == 409
    _, receipt = mutate("unstage", all=["staged"])
    completed(receipt, "unstaged")
    current = status()
    entry = next(item for item in current["entries"] if item["path"] == "README.md")
    _, receipt = mutate("stage", files=[{key: entry[key] for key in ("pathKey", "area", "fingerprint")}])
    completed(receipt, "staged")
    # A commit includes the reviewed index and leaves later work on disk.
    (repo / "README.md").write_text("Later unstaged\n")
    _, receipt = mutate("commit", summary="Reviewed commit", description="Live gateway verification")
    completed(receipt, "committed")
    assert git(repo, "show", "HEAD:README.md") == "Modified"
    assert (repo / "README.md").read_text() == "Later unstaged\n"
    stale = status()["preconditions"]
    (repo / "another.txt").write_text("external edit\n")
    _, receipt = mutate("stage", expected=stale, all=["untracked"])
    assert receipt["outcome"]["code"] == "refresh_required"
    _, receipt = mutate("stage", all=["unstaged", "untracked"])
    completed(receipt)
    _, receipt = mutate("commit", summary="Prepare branch verification")
    completed(receipt)
    current = status()
    _, receipt = mutate("branches/create", name="verification/topic", startPoint="main", startOid=current["head"]["oid"], switch=True)
    completed(receipt)
    _, receipt = mutate("branches/switch", name="main", oid=current["head"]["oid"])
    completed(receipt)
    assert api("/v2/git/branches", path=str(repo))[0] == 200
    _, preview = api("/v2/git/push/preview", path=str(repo))
    source, target = preview["source"], preview["target"]
    _, receipt = mutate("push", expected=preview["preconditions"], remote=target["remote"], remoteUrl=target["remoteUrl"],
                        destination=target["destination"], sourceBranch=source["branch"], sourceOid=source["oid"], destinationOid=target["destinationOid"])
    completed(receipt, "pushed")
    # Explicit fetch and fast-forward. Reads must not discover this commit.
    git(other, "fetch", "-q", "origin")
    git(other, "merge", "--ff-only", "origin/main")
    commit(other, "REMOTE.md", "remote\n", "Remote update")
    git(other, "push", "-q", "origin", "main")
    before_fetch = status()
    assert before_fetch["upstream"]["behind"] == 0
    _, receipt = mutate("fetch", remote="origin")
    completed(receipt, "fetched")
    _, sync = api("/v2/git/sync", path=str(repo))
    assert sync["relation"] == "behind" and len(sync["incoming"]) == 1
    _, receipt = mutate("fast-forward", expected=sync["preconditions"], upstream=sync["upstream"]["name"], upstreamOid=sync["upstream"]["oid"])
    completed(receipt)
    # Prepare a real conflict in another disposable worktree.
    conflict = base / "conflict"
    git(base, "clone", "-q", str(bare), str(conflict))
    configure(conflict)
    commit(conflict, "README.md", "Local conflict\n", "Local conflict")
    commit(other, "README.md", "Remote conflict\n", "Remote conflict")
    git(other, "push", "-q", "origin", "main")
    _, receipt = mutate("fetch", path=conflict, remote="origin")
    completed(receipt)
    _, sync = api("/v2/git/sync", path=str(conflict))
    assert sync["relation"] == "diverged"
    update = dict(upstream=sync["upstream"]["name"], upstreamOid=sync["upstream"]["oid"])
    _, receipt = mutate("fast-forward", path=conflict, **update)
    assert receipt["outcome"]["code"] == "not_fast_forward"
    _, receipt = mutate("merge", path=conflict, **update)
    completed(receipt, "stopped")
    _, receipt = mutate("continue", path=conflict, operation="merge")
    assert receipt["outcome"]["code"] == "unresolved_conflicts"
    current = status(conflict)
    entry = next(item for item in current["entries"] if item["area"] == "conflicted")
    files = [{key: entry[key] for key in ("pathKey", "area", "fingerprint")}]
    _, receipt = mutate("stage", path=conflict, files=files)
    assert receipt["outcome"]["code"] == "conflict_markers"
    (conflict / "README.md").write_text("Resolved on the Mac\n")
    current = status(conflict)
    entry = next(item for item in current["entries"] if item["area"] == "conflicted")
    _, receipt = mutate("stage", path=conflict, files=[{key: entry[key] for key in ("pathKey", "area", "fingerprint")}])
    completed(receipt)
    _, receipt = mutate("continue", path=conflict, operation="merge")
    completed(receipt)
    # A normal Mac commit hook must not smuggle unreviewed staging into a commit.
    hook_repo = base / "hook-review"
    hook_repo.mkdir()
    git(hook_repo, "init", "-qb", "main")
    configure(hook_repo)
    commit(hook_repo, "README.md", "initial\n", "Initial")
    (hook_repo / "reviewed.txt").write_text("reviewed\n")
    (hook_repo / "extra.txt").write_text("not reviewed\n")
    git(hook_repo, "add", "reviewed.txt")
    hook = hook_repo / ".git/hooks/pre-commit"
    hook.write_text("#!/bin/sh\ngit add -- extra.txt\n")
    hook.chmod(0o700)
    before_hook = git(hook_repo, "rev-parse", "HEAD")
    before_index = git(hook_repo, "write-tree")
    _, receipt = mutate("commit", path=hook_repo, summary="Reviewed index only")
    assert receipt["outcome"]["code"] == "refresh_required"
    assert git(hook_repo, "rev-parse", "HEAD") == before_hook
    assert git(hook_repo, "write-tree") == before_index
    # Keep readable screen fixtures: staged, unstaged, untracked, ahead/behind.
    (repo / "README.md").write_text("Staged review\n")
    git(repo, "add", "README.md")
    (repo / "README.md").write_text("Staged review\nUnstaged work\n")
    (repo / "new file.txt").write_text("New file\n")
    commit(repo, "LOCAL.md", "local\n", "Outgoing for simulator")
    _, receipt = mutate("fetch", remote="origin")
    completed(receipt)
    (repo / "README.md").write_text("Review this staged change\n")
    git(repo, "add", "README.md")
    (repo / "README.md").write_text("Review this staged change\nKeep this unstaged\n")
    # A second conflict left stopped for simulator inspection.
    conflict_screen = base / "conflict-screen"
    git(base, "clone", "-q", str(bare), str(conflict_screen))
    configure(conflict_screen)
    commit(conflict_screen, "README.md", "Screen local\n", "Local conflict screen")
    commit(other, "README.md", "Screen remote\n", "Remote conflict screen")
    git(other, "push", "-q", "origin", "main")
    _, receipt = mutate("fetch", path=conflict_screen, remote="origin")
    completed(receipt)
    _, sync = api("/v2/git/sync", path=str(conflict_screen))
    _, receipt = mutate("merge", path=conflict_screen, upstream=sync["upstream"]["name"], upstreamOid=sync["upstream"]["oid"])
    completed(receipt, "stopped")
    # A live session changes directory after launch. Git grouping must use
    # its actual child cwd, without taking a terminal surface.
    session_env = {key: value for key, value in env.items() if not key.startswith("LATCH_")}
    session_env["LATCH_HOME"] = str(home)
    manifest = {"format_version": 1, "launch": {"argv": ["/bin/sh", "-c", 'cd -- "$1"; exec /bin/sleep 300', "verify", str(repo)],
                "cwd": str(base), "size": {"cols": 80, "rows": 24}}, "display": {"name": "verify-directory-move"}}
    launched = subprocess.run([str(ROOT / "target/debug/latch"), "create", "--manifest-file", "-", "--json"],
                              input=json.dumps(manifest), env=session_env, text=True, capture_output=True, check=True)
    session_id = json.loads(launched.stdout)["session"]["id"]
    try:
        for _ in range(30):
            mapped = status()["sessions"]
            if any(item["id"] == session_id for item in mapped):
                break
            time.sleep(0.1)
        assert any(item["id"] == session_id for item in mapped), "directory change was not reflected"
    finally:
        subprocess.run([str(ROOT / "target/debug/latch"), "stop", session_id], env=session_env,
                       capture_output=True, check=True)
    assert all(item["id"] != session_id for item in status()["sessions"])
    runtime = dict(base=str(base), home=str(home), repo=str(repo), conflict=str(conflict_screen), url=url, token=token, pid=server.pid)
    (base / "runtime.json").write_text(json.dumps(runtime))
    (base / "runtime.json").chmod(0o600)
    (OUT / "live-gateway.json").write_text(json.dumps({"fixture": str(base), "events": events, "result": "pass"}, indent=2) + "\n")
    print(json.dumps({"result": "pass", "requests": len(events), "runtime": str(base / "runtime.json")}), flush=True)
    if "--keep" in sys.argv:
        signal.pause()
finally:
    server.terminate()
    server.wait(timeout=10)
    gateway_log.close()
